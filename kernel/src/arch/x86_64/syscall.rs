//! Syscall entry point via the SYSCALL/SYSRET instruction pair.
//!
//! On SYSCALL the CPU:
//!   - saves RIP → RCX, RFLAGS → R11
//!   - switches CS/SS per the STAR MSR
//!   - does NOT change RSP (still user RSP)
//!
//! The entry stub manually switches to the kernel syscall stack, saves
//! callee-saved registers, calls the Rust dispatcher, restores registers,
//! restores user RSP, and returns with SYSRETQ.
//!
//! # Syscall table (Phase 11)
//!
//! | Number | Name         | Args                  |
//! |---|---|---|
//! | 1–5,7–10 | IPC     | (dispatched to ipc::dispatch) |
//! | 6       | exit (legacy) | code                |
//! | 12      | debug_print   | ptr, len            |
//! | 39      | getpid        | —                   |
//! | 57      | fork          | —                   |
//! | 59      | execve        | path_ptr, path_len  |
//! | 60      | exit          | code                |
//! | 61      | waitpid       | pid, status_ptr     |
//! | 110     | getppid       | —                   |
//! | 231     | exit_group    | code                |

extern crate alloc;

// Linux errno values (negated for syscall return convention).
#[allow(dead_code)]
const NEG_EPERM: u64 = (-1_i64) as u64;
const NEG_ENOENT: u64 = (-2_i64) as u64;
const NEG_EIO: u64 = (-5_i64) as u64;
const NEG_EBADF: u64 = (-9_i64) as u64;
#[allow(dead_code)]
const NEG_EAGAIN: u64 = (-11_i64) as u64;
const NEG_EFAULT: u64 = (-14_i64) as u64;
const NEG_EINVAL: u64 = (-22_i64) as u64;
const NEG_EMFILE: u64 = (-24_i64) as u64;
const NEG_EEXIST: u64 = (-17_i64) as u64;
const NEG_ENOSPC: u64 = (-28_i64) as u64;
const NEG_EROFS: u64 = (-30_i64) as u64;
const NEG_ENOTDIR: u64 = (-20_i64) as u64;
const NEG_EISDIR: u64 = (-21_i64) as u64;
const NEG_ENOSYS: u64 = (-38_i64) as u64;
const NEG_ESRCH: u64 = (-3_i64) as u64;
const NEG_EINTR: u64 = (-4_i64) as u64;
const NEG_ENOTEMPTY: u64 = (-39_i64) as u64;
const NEG_ENOMEM: u64 = (-12_i64) as u64;
#[allow(dead_code)]
const NEG_ELOOP: u64 = (-40_i64) as u64;
#[allow(dead_code)]
const NEG_EXDEV: u64 = (-18_i64) as u64;
const NEG_EBUSY: u64 = (-16_i64) as u64;
const NEG_ENXIO: u64 = (-6_i64) as u64;

/// linux_dirent64 type constants.
#[allow(dead_code)]
const DT_DIR: u8 = 4;
#[allow(dead_code)]
const DT_LNK: u8 = 10;
#[allow(dead_code)]
const DT_REG: u8 = 8;

const EXT2_SUPER_MAGIC: i64 = 0xEF53;
const TMPFS_MAGIC: i64 = 0x0102_1994;
const PROC_SUPER_MAGIC: i64 = 0x0000_9FA0;
const RAMFS_MAGIC: i64 = 0x8584_58F6u32 as i64;
const MSDOS_SUPER_MAGIC: i64 = 0x0000_4D44;
const PIPEFS_MAGIC: i64 = 0x5049_5045;
const SOCKFS_MAGIC: i64 = 0x534F_434B;
const STATFS_BLOCK_SIZE: i64 = 4096;
const STATFS_NAME_MAX: i64 = 255;
const TMPFS_TOTAL_BLOCKS: u64 =
    (crate::fs::tmpfs::MAX_FILE_SIZE as u64).div_ceil(STATFS_BLOCK_SIZE as u64);
const TMPFS_TOTAL_FILES: u64 = 1024;
const VIRTUAL_FS_DEFAULT_BLOCKS: u64 = 1024;
const VIRTUAL_FS_DEFAULT_FILES: u64 = 1024;
static MOUNT_OP_LOCK: spin::Mutex<()> = spin::Mutex::new(());

#[repr(C)]
struct Statfs {
    f_type: i64,
    f_bsize: i64,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
    f_fsid: [i32; 2],
    f_namelen: i64,
    f_frsize: i64,
    f_flags: i64,
    f_spare: [i64; 4],
}

const _: [(); 120] = [(); core::mem::size_of::<Statfs>()];

// ---------------------------------------------------------------------------
// Path resolution helpers (Phase 18)
// ---------------------------------------------------------------------------

/// Resolve a path relative to the given working directory.
/// Absolute paths (starting with '/') are used as-is.
/// Relative paths are joined with cwd.
/// Normalizes `.` and `..` components.
fn resolve_path(cwd: &str, path: &str) -> alloc::string::String {
    use alloc::string::String;
    use alloc::vec::Vec;

    let combined = if path.starts_with('/') {
        String::from(path)
    } else if path.is_empty() || path == "." {
        String::from(cwd)
    } else {
        alloc::format!("{}/{}", cwd.trim_end_matches('/'), path)
    };

    let mut parts: Vec<&str> = Vec::new();
    for component in combined.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }

    if parts.is_empty() {
        String::from("/")
    } else {
        let mut result = String::new();
        for part in &parts {
            result.push('/');
            result.push_str(part);
        }
        result
    }
}

/// Get the current process's working directory.
fn current_cwd() -> alloc::string::String {
    let pid = crate::process::current_pid();
    let table = crate::process::PROCESS_TABLE.lock();
    match table.find(pid) {
        Some(p) => p.cwd.clone(),
        None => alloc::string::String::from("/"),
    }
}

fn current_umask() -> u16 {
    let pid = crate::process::current_pid();
    let table = crate::process::PROCESS_TABLE.lock();
    table.find(pid).map(|proc| proc.umask).unwrap_or(0o022)
}

enum PathNodeKind {
    File,
    Dir,
    Symlink(alloc::string::String),
}

fn parent_path(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/",
        Some((parent, _)) => parent,
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn path_node_nofollow(abs_path: &str) -> Result<PathNodeKind, u64> {
    if abs_path == "/" || abs_path == "/tmp" || abs_path == "/dev" || abs_path == "/dev/pts" {
        return Ok(PathNodeKind::Dir);
    }
    if let Some(node) = crate::fs::procfs::path_node(abs_path) {
        return match node {
            crate::fs::procfs::ProcfsNode::Dir => Ok(PathNodeKind::Dir),
            crate::fs::procfs::ProcfsNode::File => Ok(PathNodeKind::File),
            crate::fs::procfs::ProcfsNode::Symlink(target) => Ok(PathNodeKind::Symlink(target)),
        };
    }
    if abs_path == "/dev/null"
        || abs_path == "/dev/zero"
        || abs_path == "/dev/urandom"
        || abs_path == "/dev/random"
        || abs_path == "/dev/full"
        || abs_path == "/dev/ptmx"
    {
        return Ok(PathNodeKind::File);
    }
    if abs_path.starts_with("/dev/pts/") {
        return if dev_pts_path_exists(abs_path) {
            Ok(PathNodeKind::File)
        } else {
            Err(NEG_ENOENT)
        };
    }

    if let Some(rel) = tmpfs_relative_path(abs_path) {
        if rel.is_empty() {
            return Ok(PathNodeKind::Dir);
        }
        let tmpfs = crate::fs::tmpfs::TMPFS.lock();
        return match tmpfs.stat(rel) {
            Ok(stat) if stat.is_symlink => tmpfs
                .read_symlink(rel)
                .map(|target| PathNodeKind::Symlink(alloc::string::String::from(target)))
                .map_err(|_| NEG_EIO),
            Ok(stat) if stat.is_dir => Ok(PathNodeKind::Dir),
            Ok(_) => Ok(PathNodeKind::File),
            Err(crate::fs::tmpfs::TmpfsError::NotFound) => Err(NEG_ENOENT),
            Err(crate::fs::tmpfs::TmpfsError::NotADirectory) => Err(NEG_ENOTDIR),
            Err(_) => Err(NEG_EIO),
        };
    }

    if let Some(node) = crate::fs::ramdisk::ramdisk_lookup(abs_path) {
        return if node.is_dir() {
            Ok(PathNodeKind::Dir)
        } else {
            Ok(PathNodeKind::File)
        };
    }

    if abs_path == "/data" {
        return Ok(PathNodeKind::Dir);
    }
    if let Some(rel) = fat32_relative_path(abs_path) {
        if crate::fs::ext2::is_mounted() {
            let vol = crate::fs::ext2::EXT2_VOLUME.lock();
            if let Some(vol) = vol.as_ref()
                && let Ok(ino) = vol.resolve_path(rel)
                && let Ok(inode) = vol.read_inode(ino)
            {
                return if inode.is_symlink() {
                    vol.read_symlink(ino)
                        .map(PathNodeKind::Symlink)
                        .map_err(|_| NEG_EIO)
                } else if inode.is_dir() {
                    Ok(PathNodeKind::Dir)
                } else {
                    Ok(PathNodeKind::File)
                };
            }
        }
        if rel.is_empty() {
            return if data_is_mounted() {
                Ok(PathNodeKind::Dir)
            } else {
                Err(NEG_ENOENT)
            };
        }
        if crate::fs::fat32::is_mounted() {
            let vol = crate::fs::fat32::FAT32_VOLUME.lock();
            if let Some(vol) = vol.as_ref() {
                return match vol.lookup(rel) {
                    Ok(entry) if entry.is_dir() => Ok(PathNodeKind::Dir),
                    Ok(_) => Ok(PathNodeKind::File),
                    Err(_) => Err(NEG_ENOENT),
                };
            }
        }
    }

    if crate::fs::ext2::is_mounted()
        && let Some(rel) = ext2_root_path(abs_path)
    {
        let vol = crate::fs::ext2::EXT2_VOLUME.lock();
        if let Some(vol) = vol.as_ref() {
            return match vol.resolve_path(rel) {
                Ok(ino) => match vol.read_inode(ino) {
                    Ok(inode) if inode.is_symlink() => vol
                        .read_symlink(ino)
                        .map(PathNodeKind::Symlink)
                        .map_err(|_| NEG_EIO),
                    Ok(inode) if inode.is_dir() => Ok(PathNodeKind::Dir),
                    Ok(_) => Ok(PathNodeKind::File),
                    Err(_) => Err(NEG_EIO),
                },
                Err(kernel_core::fs::ext2::Ext2Error::NotFound) => Err(NEG_ENOENT),
                Err(kernel_core::fs::ext2::Ext2Error::NotDirectory) => Err(NEG_ENOTDIR),
                Err(_) => Err(NEG_EIO),
            };
        }
    }

    Err(NEG_ENOENT)
}

fn resolve_existing_fs_path(
    abs_path: &str,
    follow_final: bool,
) -> Result<alloc::string::String, u64> {
    let mut current = resolve_path("/", abs_path);
    let mut hops = 0usize;

    'restart: loop {
        let parts: alloc::vec::Vec<&str> = current
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();
        if parts.is_empty() {
            return Ok(alloc::string::String::from("/"));
        }

        let mut prefix = alloc::string::String::from("/");
        for (index, part) in parts.iter().enumerate() {
            require_search_permission(&prefix)?;
            let candidate = if prefix == "/" {
                alloc::format!("/{}", part)
            } else {
                alloc::format!("{}/{}", prefix, part)
            };
            let is_final = index + 1 == parts.len();
            match path_node_nofollow(&candidate)? {
                PathNodeKind::Symlink(target) if !is_final || follow_final => {
                    if hops >= 40 {
                        return Err(NEG_ELOOP);
                    }
                    hops += 1;
                    let remaining = if is_final {
                        alloc::string::String::new()
                    } else {
                        parts[index + 1..].join("/")
                    };
                    let base = if target.starts_with('/') {
                        target
                    } else {
                        resolve_path(parent_path(&candidate), &target)
                    };
                    current = if remaining.is_empty() {
                        base
                    } else {
                        resolve_path(&base, &remaining)
                    };
                    continue 'restart;
                }
                PathNodeKind::Dir if !is_final => prefix = candidate,
                PathNodeKind::File | PathNodeKind::Symlink(_) if !is_final => {
                    return Err(NEG_ENOTDIR);
                }
                _ => prefix = candidate,
            }
        }

        return Ok(prefix);
    }
}

fn resolve_parent_components(abs_path: &str) -> Result<alloc::string::String, u64> {
    let normalized = resolve_path("/", abs_path);
    let name = basename(&normalized);
    let parent = parent_path(&normalized);
    let resolved_parent = resolve_existing_fs_path(parent, true)?;
    if resolved_parent == "/" {
        Ok(alloc::format!("/{}", name))
    } else {
        Ok(alloc::format!("{}/{}", resolved_parent, name))
    }
}

fn resolve_path_from_dirfd(dirfd: u64, raw_path: &str) -> Result<alloc::string::String, u64> {
    if raw_path.is_empty() {
        return Err(NEG_ENOENT);
    }
    if raw_path.starts_with('/') {
        return Ok(resolve_path("/", raw_path));
    }
    if dirfd == AT_FDCWD {
        return Ok(resolve_path(&current_cwd(), raw_path));
    }

    let dirfd_idx = dirfd as usize;
    if dirfd_idx >= MAX_FDS {
        return Err(NEG_EBADF);
    }
    let dir_entry = current_fd_entry(dirfd_idx).ok_or(NEG_EBADF)?;
    let base = match &dir_entry.backend {
        FdBackend::Dir { path } => path.clone(),
        _ => return Err(NEG_ENOTDIR),
    };
    Ok(resolve_path(&base, raw_path))
}

fn require_search_permission(abs_path: &str) -> Result<(), u64> {
    if let Some((uid, gid, mode)) = path_metadata(abs_path) {
        let (_, _, euid, egid) = current_process_ids();
        if !check_permission(uid, gid, mode, euid, egid, 1) {
            return Err(NEG_EACCES);
        }
    }
    Ok(())
}

fn resolve_create_path(lexical: &str, follow_final: bool) -> Result<alloc::string::String, u64> {
    if follow_final && let Ok(PathNodeKind::Symlink(target)) = path_node_nofollow(lexical) {
        let target_path = if target.starts_with('/') {
            resolve_path("/", &target)
        } else {
            resolve_path(parent_path(lexical), &target)
        };
        return match resolve_existing_fs_path(&target_path, true) {
            Ok(path) => Ok(path),
            Err(NEG_ENOENT) => resolve_parent_components(&target_path),
            Err(err) => Err(err),
        };
    }

    resolve_parent_components(lexical)
}

fn open_user_path(dirfd: u64, raw_path: &str, flags: u64, mode_arg: u64) -> u64 {
    let _mount_guard = MOUNT_OP_LOCK.lock();
    if raw_path.is_empty() {
        return NEG_ENOENT;
    }
    let lexical = match resolve_path_from_dirfd(dirfd, raw_path) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let follow_final = flags & O_NOFOLLOW == 0;
    let resolved = if flags & O_CREAT != 0 {
        match resolve_existing_fs_path(&lexical, follow_final) {
            Ok(path) => path,
            Err(NEG_ENOENT) => match resolve_create_path(&lexical, follow_final) {
                Ok(path) => path,
                Err(err) => return err,
            },
            Err(err) => return err,
        }
    } else {
        match resolve_existing_fs_path(&lexical, follow_final) {
            Ok(path) => path,
            Err(err) => return err,
        }
    };

    if flags & O_NOFOLLOW != 0
        && matches!(path_node_nofollow(&resolved), Ok(PathNodeKind::Symlink(_)))
    {
        return NEG_ELOOP;
    }

    open_resolved_path(&resolved, flags, mode_arg)
}

fn make_statfs(
    f_type: i64,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
) -> Statfs {
    Statfs {
        f_type,
        f_bsize: STATFS_BLOCK_SIZE,
        f_blocks,
        f_bfree,
        f_bavail,
        f_files,
        f_ffree,
        f_fsid: [f_type as i32, 0],
        f_namelen: STATFS_NAME_MAX,
        f_frsize: STATFS_BLOCK_SIZE,
        f_flags: 0,
        f_spare: [0; 4],
    }
}

fn tmpfs_statfs() -> Statfs {
    let free_blocks = crate::mm::frame_allocator::free_count() as u64;
    make_statfs(
        TMPFS_MAGIC,
        TMPFS_TOTAL_BLOCKS,
        free_blocks.min(TMPFS_TOTAL_BLOCKS),
        free_blocks.min(TMPFS_TOTAL_BLOCKS),
        TMPFS_TOTAL_FILES,
        TMPFS_TOTAL_FILES,
    )
}

fn proc_statfs() -> Statfs {
    make_statfs(
        PROC_SUPER_MAGIC,
        VIRTUAL_FS_DEFAULT_BLOCKS,
        VIRTUAL_FS_DEFAULT_BLOCKS,
        VIRTUAL_FS_DEFAULT_BLOCKS,
        VIRTUAL_FS_DEFAULT_FILES,
        VIRTUAL_FS_DEFAULT_FILES,
    )
}

fn ramdisk_statfs() -> Statfs {
    make_statfs(
        RAMFS_MAGIC,
        VIRTUAL_FS_DEFAULT_BLOCKS,
        0,
        0,
        VIRTUAL_FS_DEFAULT_FILES,
        0,
    )
}

fn pipefs_statfs() -> Statfs {
    make_statfs(
        PIPEFS_MAGIC,
        0,
        0,
        0,
        VIRTUAL_FS_DEFAULT_FILES,
        VIRTUAL_FS_DEFAULT_FILES,
    )
}

fn sockfs_statfs() -> Statfs {
    make_statfs(
        SOCKFS_MAGIC,
        0,
        0,
        0,
        VIRTUAL_FS_DEFAULT_FILES,
        VIRTUAL_FS_DEFAULT_FILES,
    )
}

fn ext2_statfs() -> Statfs {
    let ext2 = crate::fs::ext2::EXT2_VOLUME.lock();
    if let Some(vol) = ext2.as_ref() {
        return make_statfs(
            EXT2_SUPER_MAGIC,
            vol.superblock.blocks_count as u64,
            vol.superblock.free_blocks_count as u64,
            vol.superblock.free_blocks_count as u64,
            vol.superblock.inodes_count as u64,
            vol.superblock.free_inodes_count as u64,
        );
    }
    ramdisk_statfs()
}

fn fat32_statfs() -> Statfs {
    let fat32 = crate::fs::fat32::FAT32_VOLUME.lock();
    if let Some(vol) = fat32.as_ref() {
        let reserved = vol.bpb.reserved_sectors as u64;
        let fats = (vol.bpb.num_fats as u64) * (vol.bpb.fat_size_32 as u64);
        let data_sectors = (vol.bpb.total_sectors_32 as u64).saturating_sub(reserved + fats);
        let data_bytes = data_sectors * (vol.bpb.bytes_per_sector as u64);
        let total_blocks = data_bytes.div_ceil(STATFS_BLOCK_SIZE as u64);
        return make_statfs(
            MSDOS_SUPER_MAGIC,
            total_blocks,
            0,
            0,
            VIRTUAL_FS_DEFAULT_FILES,
            0,
        );
    }
    ramdisk_statfs()
}

fn write_statfs_to_user(buf_ptr: u64, stat: &Statfs) -> u64 {
    if buf_ptr == 0 {
        return NEG_EFAULT;
    }
    // SAFETY: `Statfs` is a plain repr(C) POD buffer with a compile-time size check.
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (stat as *const Statfs).cast::<u8>(),
            core::mem::size_of::<Statfs>(),
        )
    };
    if crate::mm::user_mem::copy_to_user(buf_ptr, bytes).is_err() {
        return NEG_EFAULT;
    }
    0
}

fn dev_pts_path_exists(abs_path: &str) -> bool {
    let Some(suffix) = abs_path.strip_prefix("/dev/pts/") else {
        return false;
    };
    let Ok(pty_id) = suffix.parse::<usize>() else {
        return false;
    };
    let table = crate::pty::PTY_TABLE.lock();
    table.get(pty_id).and_then(|slot| slot.as_ref()).is_some()
}

fn statfs_for_path(abs_path: &str) -> Statfs {
    if abs_path == "/proc" || abs_path.starts_with("/proc/") {
        return proc_statfs();
    }
    if abs_path == "/tmp" || abs_path.starts_with("/tmp/") {
        return tmpfs_statfs();
    }
    if abs_path == "/dev" || abs_path.starts_with("/dev/") {
        return ramdisk_statfs();
    }
    if abs_path == "/" {
        return if crate::fs::ext2::is_mounted() {
            ext2_statfs()
        } else {
            ramdisk_statfs()
        };
    }
    if crate::fs::ramdisk::ramdisk_lookup(abs_path).is_some() {
        return ramdisk_statfs();
    }
    if crate::fs::ext2::is_mounted()
        && let Some(rel) = ext2_root_path(abs_path)
    {
        let vol = crate::fs::ext2::EXT2_VOLUME.lock();
        if let Some(vol) = vol.as_ref()
            && vol.exists(rel)
        {
            return ext2_statfs();
        }
    }
    if let Some(rel) = fat32_relative_path(abs_path) {
        if crate::fs::ext2::is_mounted() {
            let vol = crate::fs::ext2::EXT2_VOLUME.lock();
            if let Some(vol) = vol.as_ref()
                && vol.exists(rel)
            {
                return ext2_statfs();
            }
        }
        if rel.is_empty() {
            return fat32_statfs();
        }
        if crate::fs::fat32::is_mounted() {
            let vol = crate::fs::fat32::FAT32_VOLUME.lock();
            if let Some(vol) = vol.as_ref()
                && vol.lookup(rel).is_ok()
            {
                return fat32_statfs();
            }
        }
    }
    ramdisk_statfs()
}

fn statfs_path_exists(abs_path: &str) -> bool {
    if abs_path == "/" || abs_path == "/tmp" {
        return true;
    }
    if crate::fs::procfs::path_exists(abs_path) {
        return true;
    }
    if abs_path == "/dev"
        || abs_path == "/dev/pts"
        || abs_path == "/dev/null"
        || abs_path == "/dev/zero"
        || abs_path == "/dev/urandom"
        || abs_path == "/dev/random"
        || abs_path == "/dev/full"
        || abs_path == "/dev/ptmx"
    {
        return true;
    }
    if abs_path.starts_with("/dev/pts/") {
        return dev_pts_path_exists(abs_path);
    }
    if let Some(rel) = tmpfs_relative_path(abs_path) {
        if rel.is_empty() {
            return true;
        }
        let tmpfs = crate::fs::tmpfs::TMPFS.lock();
        return tmpfs.stat(rel).is_ok();
    }
    if let Some(node) = crate::fs::ramdisk::ramdisk_lookup(abs_path) {
        return node.is_dir() || node.is_file();
    }
    if crate::fs::ext2::is_mounted()
        && let Some(rel) = ext2_root_path(abs_path)
    {
        let vol = crate::fs::ext2::EXT2_VOLUME.lock();
        if let Some(vol) = vol.as_ref() {
            return vol.exists(rel);
        }
    }
    if let Some(rel) = fat32_relative_path(abs_path) {
        if crate::fs::ext2::is_mounted() {
            let vol = crate::fs::ext2::EXT2_VOLUME.lock();
            if let Some(vol) = vol.as_ref()
                && vol.exists(rel)
            {
                return true;
            }
        }
        if rel.is_empty() {
            return data_is_mounted();
        }
        if crate::fs::fat32::is_mounted() {
            let vol = crate::fs::fat32::FAT32_VOLUME.lock();
            if let Some(vol) = vol.as_ref() {
                return vol.lookup(rel).is_ok();
            }
        }
    }
    false
}

use core::arch::global_asm;

use x86_64::{
    VirtAddr,
    registers::{
        model_specific::{Efer, EferFlags, LStar, SFMask, Star},
        rflags::RFlags,
    },
};

use super::gdt;

// ---------------------------------------------------------------------------
// Statics accessed from assembly
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Per-core syscall state (Phase 35)
// ---------------------------------------------------------------------------
//
// All syscall user-state storage has moved to `PerCoreData` (smp/mod.rs).
// The assembly entry stub accesses them via `gs:[OFFSET]` (gs_base is always
// PerCoreData — user code cannot change it: no FSGSBASE, no wrmsr in ring 3).
// The Rust-side helpers below read from per-core data.

/// Read the per-core `syscall_arg3` (R10 at SYSCALL entry).
fn per_core_syscall_arg3() -> u64 {
    crate::smp::per_core().syscall_arg3
}

/// Read the per-core `syscall_stack_top`.
pub(crate) fn per_core_syscall_stack_top() -> u64 {
    crate::smp::per_core().syscall_stack_top
}

/// Read the per-core `syscall_user_rsp`.
pub(crate) fn per_core_syscall_user_rsp() -> u64 {
    crate::smp::per_core().syscall_user_rsp
}

/// Update the per-core `syscall_stack_top` (e.g. on process switch).
///
/// # Safety
///
/// Must only be called on the owning core.
pub(crate) unsafe fn set_per_core_syscall_stack_top(val: u64) {
    let data =
        crate::smp::per_core() as *const crate::smp::PerCoreData as *mut crate::smp::PerCoreData;
    unsafe {
        (*data).syscall_stack_top = val;
    }
}

// ---------------------------------------------------------------------------
// Assembly entry stub
// ---------------------------------------------------------------------------

global_asm!(
    // Per-core field offsets (computed at compile time via offset_of!).
    ".equ OFF_STACK_TOP,   {off_stack_top}",
    ".equ OFF_USER_RSP,    {off_user_rsp}",
    ".equ OFF_ARG3,        {off_arg3}",
    ".equ OFF_USER_RBX,    {off_user_rbx}",
    ".equ OFF_USER_RBP,    {off_user_rbp}",
    ".equ OFF_USER_R12,    {off_user_r12}",
    ".equ OFF_USER_R13,    {off_user_r13}",
    ".equ OFF_USER_R14,    {off_user_r14}",
    ".equ OFF_USER_R15,    {off_user_r15}",
    ".equ OFF_USER_RDI,    {off_user_rdi}",
    ".equ OFF_USER_RSI,    {off_user_rsi}",
    ".equ OFF_USER_RDX,    {off_user_rdx}",
    ".equ OFF_USER_R8,     {off_user_r8}",
    ".equ OFF_USER_R9,     {off_user_r9}",
    ".equ OFF_USER_R10,    {off_user_r10}",
    ".equ OFF_USER_RFLAGS, {off_user_rflags}",

    ".global syscall_entry",
    "syscall_entry:",
    // At entry (from ring 3 via SYSCALL):
    //   RSP  = user RSP
    //   RCX  = user RIP       (return address for SYSRETQ)
    //   R11  = user RFLAGS
    //   RAX  = syscall number
    //   RDI/RSI/RDX = args 0-2
    //   GS_BASE = PerCoreData (user cannot change it: no FSGSBASE, no wrmsr)

    // --- Switch to per-core kernel stack ---
    "mov gs:[OFF_USER_RSP], rsp",
    "mov rsp, gs:[OFF_STACK_TOP]",
    "cld",

    // --- Save user callee-saved registers to per-core data ---
    "mov gs:[OFF_USER_RBX], rbx",
    "mov gs:[OFF_USER_RBP], rbp",
    "mov gs:[OFF_USER_R12], r12",
    "mov gs:[OFF_USER_R13], r13",
    "mov gs:[OFF_USER_R14], r14",
    "mov gs:[OFF_USER_R15], r15",

    // --- Save user caller-saved registers (Linux ABI preserves these) ---
    "mov gs:[OFF_USER_RDI], rdi",
    "mov gs:[OFF_USER_RSI], rsi",
    "mov gs:[OFF_USER_RDX], rdx",
    "mov gs:[OFF_USER_R8],  r8",
    "mov gs:[OFF_USER_R9],  r9",
    "mov gs:[OFF_USER_R10], r10",
    "mov gs:[OFF_USER_RFLAGS], r11",

    // --- Save return address and user flags on stack ---
    "push rcx", // user RIP
    "push r11", // user RFLAGS

    // --- Save callee-saved registers on stack ---
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",

    // --- Save caller-saved registers on stack (Linux-preserved) ---
    "push rdi",
    "push rsi",
    "push rdx",
    "push r10",
    "push r8",
    "push r9",

    // --- Set up SysV arguments for syscall_handler ---
    // Save r10 (arg3) to per-core data for kernel-side access.
    "mov gs:[OFF_ARG3], r10",
    // Load r8 (user_rip) BEFORE overwriting rcx.
    "mov r8, [rsp + 104]",         // user_rip (5th param)
    "mov r9, gs:[OFF_USER_RSP]",   // user_rsp (6th param)
    "mov rcx, rdx",                // arg2
    "mov rdx, rsi",                // arg1
    "mov rsi, rdi",                // arg0
    "mov rdi, rax",                // syscall number
    "call syscall_handler",
    // Return value is in RAX.

    // --- Restore caller-saved registers (Linux-preserved) ---
    "pop r9",
    "pop r8",
    "pop r10",
    "pop rdx",
    "pop rsi",
    "pop rdi",
    // --- Restore callee-saved registers ---
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    // --- Restore return info ---
    "pop r11", // user RFLAGS
    "pop rcx", // user RIP

    // --- Restore user RSP and return ---
    "mov rsp, gs:[OFF_USER_RSP]",
    "sysretq",

    off_stack_top   = const crate::smp::offsets::SYSCALL_STACK_TOP,
    off_user_rsp    = const crate::smp::offsets::SYSCALL_USER_RSP,
    off_arg3        = const crate::smp::offsets::SYSCALL_ARG3,
    off_user_rbx    = const crate::smp::offsets::SYSCALL_USER_RBX,
    off_user_rbp    = const crate::smp::offsets::SYSCALL_USER_RBP,
    off_user_r12    = const crate::smp::offsets::SYSCALL_USER_R12,
    off_user_r13    = const crate::smp::offsets::SYSCALL_USER_R13,
    off_user_r14    = const crate::smp::offsets::SYSCALL_USER_R14,
    off_user_r15    = const crate::smp::offsets::SYSCALL_USER_R15,
    off_user_rdi    = const crate::smp::offsets::SYSCALL_USER_RDI,
    off_user_rsi    = const crate::smp::offsets::SYSCALL_USER_RSI,
    off_user_rdx    = const crate::smp::offsets::SYSCALL_USER_RDX,
    off_user_r8     = const crate::smp::offsets::SYSCALL_USER_R8,
    off_user_r9     = const crate::smp::offsets::SYSCALL_USER_R9,
    off_user_r10    = const crate::smp::offsets::SYSCALL_USER_R10,
    off_user_rflags = const crate::smp::offsets::SYSCALL_USER_RFLAGS,
);

// ---------------------------------------------------------------------------
// Syscall dispatcher
// ---------------------------------------------------------------------------

/// Linux syscall number → handler dispatch (Phase 12, T011–T026).
///
/// Numbers that happen to match our Phase 11 ABI are handled identically.
/// The custom debug-print syscall is moved from 12 → 0x1000 to free up
/// Linux brk = 12.
///
/// # Syscall audit (T011) — Linux numbers that musl requires
///
/// | Linux # | Name        | Implementation        |
/// |---------|-------------|----------------------|
/// |       0 | read        | ramdisk / stdin stub  |
/// |       1 | write       | stdout → serial       |
/// |       2 | open        | ramdisk lookup        |
/// |       3 | close       | fd-table release      |
/// |       5 | fstat       | minimal stat struct   |
/// |       8 | lseek       | per-fd offset update  |
/// |       9 | mmap        | anonymous only        |
/// |      11 | munmap      | stub (no-op)          |
/// |      12 | brk         | frame-backed heap     |
/// |      16 | ioctl       | TIOCGWINSZ only       |
/// |      19 | readv       | loop over read        |
/// |      20 | writev      | loop over write       |
/// |      39 | getpid      | ✓ same as Phase 11    |
/// |      57 | fork        | ✓ same as Phase 11    |
/// |      59 | execve      | ✓ same as Phase 11    |
/// |      60 | exit        | ✓ same as Phase 11    |
/// |      61 | wait4       | ✓ waitpid Phase 11    |
/// |      63 | uname       | fixed identity string |
/// |      79 | getcwd      | always returns "/"    |
/// |      80 | chdir       | stub (always ok)      |
/// |     110 | getppid     | ✓ same as Phase 11    |
/// |     158 | arch_prctl  | ARCH_SET_FS only       |
/// |     218 | set_tid_addr| stub, returns PID      |
/// |     231 | exit_group  | ✓ kills all threads   |
/// |     257 | openat      | delegates to open     |
/// |     262 | newfstatat  | delegates to fstat    |
#[unsafe(no_mangle)]
pub extern "C" fn syscall_handler(
    number: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    user_rip: u64,
    user_rsp: u64,
) -> u64 {
    // Divergent syscalls (exit, sigreturn) never return — handle them first.
    match number {
        15 => sys_sigreturn(user_rsp),
        60 => sys_exit(arg0 as i32),
        231 => sys_exit_group(arg0 as i32),
        _ => {}
    }

    let result = match number {
        // Linux-compatible file I/O (Phase 12, T013–T017)
        0 => sys_linux_read(arg0, arg1, arg2),
        1 => sys_linux_write(arg0, arg1, arg2),
        2 => sys_linux_open(arg0, arg1, arg2),
        3 => sys_linux_close(arg0),
        // stat(path, buf) follows the final symlink.
        4 => sys_linux_fstatat(AT_FDCWD, arg0, arg1, 0),
        5 => sys_linux_fstat(arg0, arg1),
        // lstat(path, buf) inspects the final symlink itself.
        6 => sys_linux_fstatat(AT_FDCWD, arg0, arg1, AT_SYMLINK_NOFOLLOW),
        // Phase 22: poll stub — report all requested fds as ready.
        // Ion uses poll() to multiplex between signal pipe and stdin.
        7 => sys_poll(arg0, arg1, arg2),
        8 => sys_linux_lseek(arg0, arg1, arg2),
        // Phase 36: mprotect(addr, len, prot) — change page permissions.
        10 => sys_mprotect(arg0, arg1, arg2),
        // Linux-compatible memory (Phase 12, T018–T020)
        9 => sys_linux_mmap(arg0, arg1, arg2),
        11 => sys_linux_munmap(arg0, arg1),
        12 => sys_linux_brk(arg0),
        86 => sys_link(arg0, arg1),
        88 => sys_symlink(arg0, arg1),
        89 => sys_readlink(arg0, arg1, arg2),
        // Phase 14: signal syscalls (rt_sigaction, rt_sigprocmask)
        13 => sys_rt_sigaction(arg0, arg1, arg2),
        14 => sys_rt_sigprocmask(arg0, arg1, arg2),
        // Linux misc (Phase 12, T023–T026)
        16 => sys_linux_ioctl(arg0, arg1, arg2),
        19 => sys_linux_readv(arg0, arg1, arg2),
        20 => sys_linux_writev(arg0, arg1, arg2),
        // Phase 21: access stub (PATH search — check existence only)
        21 => sys_access(arg0),
        // Phase 37: select(nfds, readfds, writefds, exceptfds, timeout)
        23 => {
            let exceptfds = per_core_syscall_arg3();
            let timeout_ptr = crate::smp::per_core().syscall_user_r8;
            sys_select(arg0, arg1, arg2, exceptfds, timeout_ptr)
        }
        // Phase 14: pipe and dup2
        22 => sys_pipe_with_flags(arg0, false),
        32 => sys_dup(arg0),
        33 => sys_dup2(arg0, arg1),
        // Phase 35: nice(increment) — adjust task priority
        34 => {
            let pid = crate::process::current_pid();
            let uid_val = {
                let table = crate::process::PROCESS_TABLE.lock();
                table.find(pid).map(|p| p.uid).unwrap_or(0)
            };
            crate::task::sys_nice(arg0 as i32, uid_val) as u64
        }
        // Phase 14: nanosleep
        35 => sys_nanosleep(arg0),
        // Phase 23: socket syscalls
        41 => sys_socket(arg0, arg1, arg2),
        42 => sys_connect(arg0, arg1, arg2),
        43 => sys_accept(arg0, arg1, arg2),
        44 => {
            let flags = per_core_syscall_arg3();
            let addr_ptr = crate::smp::per_core().syscall_user_r8;
            let addr_len = crate::smp::per_core().syscall_user_r9;
            sys_sendto(arg0, arg1, arg2, flags, addr_ptr, addr_len)
        }
        45 => {
            let flags = per_core_syscall_arg3();
            let addr_ptr = crate::smp::per_core().syscall_user_r8;
            let addr_len_ptr = crate::smp::per_core().syscall_user_r9;
            sys_recvfrom_socket(arg0, arg1, arg2, flags, addr_ptr, addr_len_ptr)
        }
        48 => sys_shutdown_sock(arg0, arg1),
        49 => sys_bind(arg0, arg1, arg2),
        50 => sys_listen(arg0, arg1),
        51 => sys_getsockname(arg0, arg1, arg2),
        52 => sys_getpeername(arg0, arg1, arg2),
        54 => {
            let optval_ptr = per_core_syscall_arg3();
            let optlen = crate::smp::per_core().syscall_user_r8;
            sys_setsockopt(arg0, arg1, arg2, optval_ptr, optlen)
        }
        55 => {
            let optval_ptr = per_core_syscall_arg3();
            let optlen_ptr = crate::smp::per_core().syscall_user_r8;
            sys_getsockopt(arg0, arg1, arg2, optval_ptr, optlen_ptr)
        }
        // IPC syscalls (Phase 6) — kernel-task only.
        // Note: syscall 4 was IPC but is now stat (Linux ABI).
        // Note: syscall 7 was IPC but is now poll (Phase 22).
        // Note: syscall 10 was IPC but is now mprotect (Phase 21).
        // Phase 11 + Linux-compatible process syscalls
        39 => sys_getpid(),
        // Phase 39: socketpair(domain, type, protocol, sv)
        53 => {
            let sv_ptr = per_core_syscall_arg3();
            sys_socketpair(arg0, arg1, arg2, sv_ptr)
        }
        // Phase 21/40: clone — fork or thread creation
        56 => {
            let child_tidptr = per_core_syscall_arg3(); // r10
            let tls = crate::smp::per_core().syscall_user_r8;
            sys_clone(arg0, arg1, arg2, child_tidptr, tls, user_rip, user_rsp)
        }
        57 => sys_fork(user_rip, user_rsp),
        59 => sys_execve(arg0, arg1, arg2),
        61 => sys_waitpid(arg0, arg1, arg2),
        // Phase 14: signal syscalls
        62 => sys_kill(arg0, arg1),
        63 => sys_linux_uname(arg0),
        // Phase 21: fcntl stub
        72 => sys_fcntl(arg0, arg1, arg2),
        // Phase 13: filesystem mutation syscalls
        74 => sys_linux_fsync(arg0),
        // Phase 21: gettimeofday stub — return approximate time from LAPIC tick count
        96 => sys_gettimeofday(arg0),
        76 => sys_linux_truncate(arg0, arg1),
        77 => sys_linux_ftruncate(arg0, arg1),
        79 => sys_linux_getcwd(arg0, arg1),
        80 => sys_linux_chdir(arg0),
        82 => sys_linux_rename(arg0, arg1),
        83 => sys_linux_mkdir(arg0, arg1),
        84 => sys_linux_rmdir(arg0),
        87 => sys_linux_unlink(arg0),
        // Phase 27: file permission syscalls
        90 => sys_linux_chmod(arg0, arg1),
        91 => sys_linux_fchmod(arg0, arg1),
        92 => sys_linux_chown(arg0, arg1, arg2),
        93 => sys_linux_fchown(arg0, arg1, arg2),
        95 => sys_umask(arg0),
        // Phase 35: times(buf) — fill struct tms with CPU time accounting
        100 => sys_times(arg0),
        // Phase 27: user/group identity syscalls
        102 => sys_linux_getuid(),
        104 => sys_linux_getgid(),
        105 => sys_linux_setuid(arg0),
        106 => sys_linux_setgid(arg0),
        107 => sys_linux_geteuid(),
        108 => sys_linux_getegid(),
        113 => sys_linux_setreuid(arg0, arg1),
        114 => sys_linux_setregid(arg0, arg1),
        // Phase 14: process group syscalls
        109 => sys_setpgid(arg0, arg1),
        110 => sys_getppid(),
        // Phase 21: getpgrp — equivalent to getpgid(0)
        111 => sys_getpgid(0),
        // Phase 29: setsid — create a new session
        112 => sys_setsid(),
        121 => sys_getpgid(arg0),
        // Phase 29: getsid — get session ID
        124 => sys_getsid(arg0),
        // Phase 19: sigaltstack
        131 => sys_sigaltstack(arg0, arg1),
        137 => sys_statfs(arg0, arg1),
        138 => sys_fstatfs(arg0, arg1),
        // musl TLS init (Phase 12, T030 dependency)
        158 => sys_linux_arch_prctl(arg0, arg1),
        // Phase 24: mount(source, target, fstype)
        165 => sys_linux_mount(arg0, arg1, arg2),
        166 => sys_linux_umount2(arg0, arg1),
        // Phase 19: gettid — returns PID (no threads, tid=pid)
        186 => sys_gettid(),
        // Phase 40: tkill(tid, sig) — send signal to a specific thread
        200 => sys_tkill(arg0, arg1),
        // Phase 40: futex — real wait/wake queues
        202 => {
            let val3 = crate::smp::per_core().syscall_user_r9;
            sys_futex(arg0, arg1, arg2, val3)
        }
        // Phase 35: sched_setaffinity(pid, len, mask_ptr) / sched_getaffinity(pid, len, mask_ptr)
        203 => {
            // sched_setaffinity: read mask from user memory
            if arg2 == 0 {
                return NEG_EFAULT;
            }
            if arg1 < 8 {
                return NEG_EINVAL;
            }
            let mask = {
                let mut buf = [0u8; 8];
                if crate::mm::user_mem::copy_from_user(&mut buf, arg2).is_err() {
                    return NEG_EFAULT;
                }
                u64::from_ne_bytes(buf)
            };
            crate::task::sys_sched_setaffinity(arg0 as u32, mask) as u64
        }
        204 => {
            // sched_getaffinity: write mask to user memory
            let mask = crate::task::sys_sched_getaffinity(arg0 as u32);
            if mask < 0 {
                mask as u64
            } else if arg2 != 0 && arg1 >= 8 {
                let bytes = (mask as u64).to_ne_bytes();
                if crate::mm::user_mem::copy_to_user(arg2, &bytes).is_err() {
                    return NEG_EFAULT;
                }
                8 // return bytes written
            } else {
                NEG_EINVAL
            }
        }
        217 => sys_linux_getdents64(arg0, arg1, arg2),
        218 => sys_linux_set_tid_address(arg0),
        // Phase 21: clock_gettime — return approximate time from LAPIC ticks
        228 => sys_clock_gettime(arg0, arg1),
        // Phase 18: openat(dirfd, path, flags) — mode (4th arg) not yet wired through
        257 => sys_linux_openat(arg0, arg1, arg2),
        // Phase 21: set_robust_list stub — musl thread init, no-op
        273 => 0,
        // Phase 21: dup3 — delegate to dup2 (ignore flags)
        292 => sys_dup2(arg0, arg1),
        // Phase 21: pipe2 — delegate to pipe (ignore flags)
        293 => {
            // pipe2(fds, flags) — O_CLOEXEC = 0x80000
            let cloexec = arg1 & 0x80000 != 0;
            sys_pipe_with_flags(arg0, cloexec)
        }
        // Phase 37: epoll_create1(flags)
        291 => sys_epoll_create1(arg0),
        // Phase 37: accept4(fd, addr, addrlen, flags) — syscall 288
        288 => {
            let flags = per_core_syscall_arg3();
            sys_accept4(arg0, arg1, arg2, flags)
        }
        // Phase 21: prlimit64 — return ENOSYS (musl handles gracefully)
        302 => NEG_ENOSYS,
        // Phase 21: getrandom — fill buffer with TSC-seeded PRNG bytes
        318 => sys_getrandom(arg0, arg1, arg2),
        // newfstatat: fstat via path lookup
        262 => sys_linux_fstatat(arg0, arg1, arg2, per_core_syscall_arg3()),
        // Phase 37: epoll_wait(epfd, events, maxevents, timeout)
        232 => {
            let timeout = per_core_syscall_arg3();
            sys_epoll_wait(arg0, arg1, arg2, timeout)
        }
        // Phase 37: epoll_ctl(epfd, op, fd, event)
        233 => {
            let event_ptr = per_core_syscall_arg3();
            sys_epoll_ctl(arg0, arg1, arg2, event_ptr)
        }
        // Phase 37: pselect6(nfds, readfds, writefds, exceptfds, timeout, sigmask)
        270 => {
            let exceptfds = per_core_syscall_arg3();
            let timeout_ptr = crate::smp::per_core().syscall_user_r8;
            // 6th arg (sigmask) is in r9 — ignored for now.
            sys_pselect6(arg0, arg1, arg2, exceptfds, timeout_ptr)
        }
        265 => sys_linkat(
            arg0,
            arg1,
            arg2,
            per_core_syscall_arg3(),
            crate::smp::per_core().syscall_user_r8,
        ),
        266 => sys_symlinkat(arg0, arg1, arg2),
        267 => sys_readlinkat(arg0, arg1, arg2, per_core_syscall_arg3()),
        // Phase 32: utimensat(dirfd, path, times, flags) — update file timestamps
        280 => {
            let flags = per_core_syscall_arg3();
            sys_utimensat(arg0, arg1, arg2, flags)
        }
        // Custom kernel debug print (moved from 12, Phase 12 T010)
        0x1000 => sys_debug_print(arg0, arg1),
        // Custom kernel meminfo (Phase 33 Track F)
        0x1001 => sys_meminfo(arg0, arg1),
        // Phase 47 Track A: framebuffer info + mmap
        0x1002 => sys_framebuffer_info(arg0, arg1),
        0x1003 => sys_framebuffer_mmap(),
        // Phase 47 Track B: raw scancode
        0x1004 => sys_read_scancode(),
        _ => {
            // Phase 21: log unhandled syscalls to help debug ion/musl runtime.
            log::warn!("unhandled syscall {number} (args: {arg0:#x}, {arg1:#x}, {arg2:#x})");
            NEG_ENOSYS
        }
    };

    // Phase 14/19: check pending signals before returning to userspace.
    // If a user handler is delivered, this diverges and never returns.
    check_pending_signals(result);

    result
}

/// Check and deliver pending signals for the current process.
///
/// Called after every syscall (except exit/execve which diverge).
/// `syscall_result` is the return value that would be placed in RAX.
///
/// If a user handler is found, this function **diverges**: it builds a
/// sigframe on the user stack and enters ring 3 at the handler address.
/// The normal syscall return path is never reached in that case.
fn check_pending_signals(syscall_result: u64) {
    let pid = crate::process::current_pid();
    if pid == 0 {
        return; // kernel task, no signals
    }

    loop {
        let sig = crate::process::dequeue_signal(pid);
        match sig {
            None => break,
            Some((signum, disposition)) => {
                use crate::process::SignalDisposition;
                match disposition {
                    SignalDisposition::Terminate => {
                        log::info!("[p{}] killed by signal {}", pid, signum);
                        sys_exit(-(signum as i32));
                    }
                    SignalDisposition::Stop => {
                        log::info!("[p{}] stopped by signal {}", pid, signum);
                        {
                            let mut table = crate::process::PROCESS_TABLE.lock();
                            if let Some(proc) = table.find_mut(pid) {
                                proc.state = crate::process::ProcessState::Stopped;
                                proc.stop_signal = signum;
                                proc.stop_reported = false;
                            }
                        }
                        crate::process::send_sigchld_to_parent(pid);
                        let sig_saved_rsp = per_core_syscall_user_rsp();
                        while {
                            let table = crate::process::PROCESS_TABLE.lock();
                            table
                                .find(pid)
                                .map(|p| p.state == crate::process::ProcessState::Stopped)
                                .unwrap_or(false)
                        } {
                            crate::task::yield_now();
                            restore_caller_context(pid, sig_saved_rsp);
                        }
                    }
                    SignalDisposition::Continue | SignalDisposition::Ignore => {}
                    SignalDisposition::UserHandler {
                        entry,
                        mask,
                        flags,
                        restorer,
                    } => {
                        deliver_user_signal(
                            pid,
                            signum,
                            syscall_result,
                            entry,
                            mask,
                            flags,
                            restorer,
                        );
                        // deliver_user_signal diverges — never reaches here.
                    }
                }
            }
        }
    }
}

/// Build a sigframe on the user stack and enter the signal handler.
///
/// This function **never returns** — it diverges into ring 3 at the
/// handler address via `iretq`.
#[allow(clippy::too_many_arguments)]
fn deliver_user_signal(
    pid: crate::process::Pid,
    signum: u32,
    syscall_result: u64,
    handler_entry: u64,
    sa_mask: u64,
    sa_flags: u64,
    restorer: u64,
) -> ! {
    // 1. Read the interrupted user register state from the kernel stack.
    let regs = unsafe { crate::signal::read_saved_user_regs(syscall_result) };

    // 2. Read and update the process's blocked_signals; check alt stack.
    let (old_blocked, alt_stack_rsp) = {
        let mut table = crate::process::PROCESS_TABLE.lock();
        let proc = match table.find_mut(pid) {
            Some(p) => p,
            None => {
                log::warn!("[signal] deliver: pid {} gone", pid);
                sys_exit(-11); // SIGSEGV
            }
        };
        let old = proc.blocked_signals;
        // Block the delivered signal + sa_mask during handler execution.
        proc.blocked_signals |= sa_mask | (1u64 << signum);
        // SIGKILL and SIGSTOP can never be blocked.
        proc.blocked_signals &= !UNBLOCKABLE_MASK;

        // Check if we should use the alternate signal stack.
        let alt_rsp = if sa_flags & SA_ONSTACK != 0
            && proc.alt_stack_base != 0
            && proc.alt_stack_flags & crate::process::SS_DISABLE == 0
            && proc.alt_stack_flags & crate::process::SS_ONSTACK == 0
        {
            // Mark the alt stack as in use; compute top with overflow check.
            proc.alt_stack_flags |= crate::process::SS_ONSTACK;
            proc.alt_stack_base.checked_add(proc.alt_stack_size)
        } else {
            None
        };
        (old, alt_rsp)
    };

    // 3. Build the sigframe on the user stack (or alt stack).
    let frame_rsp = match crate::signal::setup_signal_frame(
        &regs,
        old_blocked,
        signum,
        restorer,
        alt_stack_rsp,
    ) {
        Some(rsp) => rsp,
        None => {
            log::warn!(
                "[p{}] signal {}: cannot build sigframe (bad user stack {:#x})",
                pid,
                signum,
                regs.rsp,
            );
            sys_exit(-11); // SIGSEGV default
        }
    };

    // Write the uc_stack into the sigframe if using alt stack (so sigreturn
    // can clear SS_ONSTACK).
    if alt_stack_rsp.is_some() {
        let table = crate::process::PROCESS_TABLE.lock();
        if let Some(proc) = table.find(pid) {
            crate::signal::write_sigframe_uc_stack(
                frame_rsp,
                proc.alt_stack_base,
                proc.alt_stack_flags,
                proc.alt_stack_size,
            );
        }
    }

    log::info!(
        "[p{}] delivering signal {} → handler {:#x}, frame_rsp={:#x}",
        pid,
        signum,
        handler_entry,
        frame_rsp,
    );

    // 4. Enter ring 3 at the handler address.
    //    RIP = handler_entry, RSP = frame_rsp, RDI = signum (first arg).
    //
    //    We use a custom iretq sequence that also sets RDI.
    unsafe { enter_signal_handler(handler_entry, frame_rsp, signum as u64, &regs) }
}

/// Enter ring 3 at `handler` with `rsp` as the stack pointer and `rdi`
/// set to the signal number (first argument to the handler).
///
/// # Safety
///
/// Same requirements as `enter_userspace`.
unsafe fn enter_signal_handler(
    handler: u64,
    rsp: u64,
    sig: u64,
    saved_regs: &crate::signal::SavedUserRegs,
) -> ! {
    unsafe {
        // Build a modified copy of the interrupted user context: RIP→handler,
        // RSP→sigframe, RDI→signal number. All other GPRs retain the
        // interrupted values so no kernel register state leaks to ring 3.
        let mut regs = *saved_regs;
        regs.rip = handler;
        regs.rsp = rsp;
        regs.rdi = sig;
        restore_and_enter_userspace(&regs)
    }
}

// ---------------------------------------------------------------------------
// sys_debug_print
// ---------------------------------------------------------------------------

fn sys_debug_print(ptr: u64, len: u64) -> u64 {
    if len > 4096 {
        return u64::MAX;
    }
    let mut buf = [0u8; 4096];
    let dst = &mut buf[..len as usize];
    if crate::mm::user_mem::copy_from_user(dst, ptr).is_err() {
        log::warn!("[sys_debug_print] invalid user pointer {:#x}+{}", ptr, len);
        return u64::MAX;
    }
    if let Ok(s) = core::str::from_utf8(dst) {
        log::info!("[userspace] {}", s.trim_end_matches('\n'));
    }
    0
}

// ---------------------------------------------------------------------------
// Phase 11 syscalls
// ---------------------------------------------------------------------------

/// `getpid()` — return the calling thread's thread-group ID (POSIX PID).
fn sys_getpid() -> u64 {
    let pid = crate::process::current_pid();
    crate::process::PROCESS_TABLE
        .lock()
        .find(pid)
        .map(|p| p.tgid as u64)
        .unwrap_or(pid as u64)
}

/// `gettid()` — return the calling thread's unique thread ID.
fn sys_gettid() -> u64 {
    let pid = crate::process::current_pid();
    crate::process::PROCESS_TABLE
        .lock()
        .find(pid)
        .map(|p| p.tid as u64)
        .unwrap_or(pid as u64)
}

/// `getppid()` — return the calling process's parent PID.
fn sys_getppid() -> u64 {
    let pid = crate::process::current_pid();
    crate::process::PROCESS_TABLE
        .lock()
        .find(pid)
        .map(|p| p.ppid as u64)
        .unwrap_or(0)
}

/// Perform clear_child_tid futex wake for the given process/thread.
///
/// Writes 0 to the userspace `clear_child_tid` address and wakes one futex
/// waiter, allowing `pthread_join` to unblock.
fn do_clear_child_tid(pid: crate::process::Pid) {
    let clear_tid_addr = {
        let table = crate::process::PROCESS_TABLE.lock();
        table.find(pid).map(|p| p.clear_child_tid).unwrap_or(0)
    };
    if clear_tid_addr != 0 {
        // Write 0u32 to the userspace address.
        let zero = 0u32.to_ne_bytes();
        let _ = crate::mm::user_mem::copy_to_user(clear_tid_addr, &zero);
        // Wake one waiter on the private futex key (0, addr) — this
        // matches musl's pthread_join which uses FUTEX_WAIT|FUTEX_PRIVATE.
        use crate::process::futex::{FUTEX_BITSET_MATCH_ANY, FUTEX_TABLE};
        let key = (0u64, clear_tid_addr);
        let to_wake = {
            let mut table = FUTEX_TABLE.lock();
            let mut wake_ids = alloc::vec::Vec::new();
            if let Some(waiters) = table.get_mut(&key) {
                if !waiters.is_empty() {
                    // Wake up to 1 waiter with matching bitset.
                    let mut i = 0;
                    while i < waiters.len() && wake_ids.is_empty() {
                        if (waiters[i].bitset & FUTEX_BITSET_MATCH_ANY) != 0 {
                            let w = waiters.remove(i);
                            w.woken.store(true, core::sync::atomic::Ordering::Release);
                            wake_ids.push(w.tid);
                        } else {
                            i += 1;
                        }
                    }
                }
                if waiters.is_empty() {
                    table.remove(&key);
                }
            }
            wake_ids
        };
        for tid in to_wake {
            let _ = crate::task::wake_task(tid);
        }
    }
}

/// Perform full process exit cleanup: close FDs, mark zombie, send SIGCHLD,
/// free page table.  Called when a single-threaded process exits or when the
/// last thread in a thread group exits.
fn do_full_process_exit(pid: crate::process::Pid, code: i32) -> ! {
    // Close all open FDs so pipe ref-counts reach 0 and EOF propagates.
    crate::process::close_all_fds_for(pid);

    // Phase 47 Track C: if this process owned the raw framebuffer, restore
    // console output so the shell is visible again.
    if crate::fb::fb_owner_pid() == pid {
        crate::fb::restore_console();
    }
    {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(proc) = table.find_mut(pid) {
            proc.state = crate::process::ProcessState::Zombie;
            proc.exit_code = Some(code);
        }
    }
    // Deliver SIGCHLD to parent (Phase 14, P14-T033a).
    crate::process::send_sigchld_to_parent(pid);

    // Read the dying process's CR3 before we switch away from it.
    let cr3_phys = {
        let table = crate::process::PROCESS_TABLE.lock();
        table.find(pid).and_then(|p| p.page_table_root)
    };
    // Restore kernel page table before yielding so the next scheduled task
    // does not inherit this process's CR3.
    crate::mm::restore_kernel_cr3();
    // Free the process's user-space page table frames now that we are back
    // on the kernel CR3 and no longer using the process's address space.
    if let Some(phys) = cr3_phys {
        crate::mm::free_process_page_table(phys.as_u64());
    }
    // Mark the kernel task as dead so the scheduler reclaims it.
    crate::task::mark_current_dead();
}

/// `exit(code)` — terminate the calling thread (syscall 60).
///
/// For single-threaded processes: full process exit (zombie, SIGCHLD, free PT).
/// For threads in a thread group:
///   - If this is the LAST thread: full process cleanup.
///   - Otherwise: minimal cleanup — remove from group, clear_child_tid wake,
///     remove Process entry, mark scheduler task as Dead.
fn sys_exit(code: i32) -> ! {
    let pid = crate::process::current_pid();
    log::info!("[p{}] exit({})", pid, code);

    if pid != 0 {
        // Phase 40: clear_child_tid futex wake.
        do_clear_child_tid(pid);

        // Check if we are in a thread group.
        let thread_group = {
            let table = crate::process::PROCESS_TABLE.lock();
            table.find(pid).and_then(|p| p.thread_group.clone())
        };

        if let Some(tg) = thread_group {
            // Remove ourselves from the thread group member list.
            let (is_last, tgid) = {
                let mut members = tg.members.lock();
                members.retain(|&tid| tid != pid);
                (members.is_empty(), tg.leader_tid)
            };

            if !is_last {
                // Non-last thread: minimal cleanup only.
                if pid == tgid {
                    // Group leader: keep as zombie placeholder so TGID lookups
                    // still work for remaining threads.
                    let mut table = crate::process::PROCESS_TABLE.lock();
                    if let Some(proc) = table.find_mut(pid) {
                        proc.state = crate::process::ProcessState::Zombie;
                    }
                } else {
                    // Non-leader thread: remove Process entry (shared resources
                    // stay alive via Arc).
                    let mut table = crate::process::PROCESS_TABLE.lock();
                    table.reap(pid);
                }
                // Restore kernel CR3 before dying — we share the address space
                // but must not leave the scheduler pointing at user CR3.
                crate::mm::restore_kernel_cr3();
                // Mark our scheduler task as Dead (do NOT free page table or fds).
                crate::task::mark_current_dead();
            }
            // Last thread: fall through to full process cleanup below.
        }
        // Single-threaded process OR last thread in group: full cleanup.
        do_full_process_exit(pid, code);
    }
    // pid == 0 (kernel context): just die.
    crate::mm::restore_kernel_cr3();
    crate::task::mark_current_dead();
}

/// `exit_group(code)` — terminate all threads in the thread group (syscall 231).
///
/// For single-threaded processes: identical to `sys_exit`.
/// For thread groups: kills all sibling threads first, then the caller does
/// full process cleanup as the last thread standing.
fn sys_exit_group(code: i32) -> ! {
    let pid = crate::process::current_pid();
    log::info!("[p{}] exit_group({})", pid, code);

    if pid != 0 {
        // Check if we are in a thread group.
        let thread_group = {
            let table = crate::process::PROCESS_TABLE.lock();
            table.find(pid).and_then(|p| p.thread_group.clone())
        };

        if let Some(tg) = thread_group {
            // Collect sibling TIDs (everyone except us).
            let siblings: alloc::vec::Vec<u32> = {
                let members = tg.members.lock();
                members.iter().copied().filter(|&tid| tid != pid).collect()
            };

            // Kill each sibling thread.
            for sibling_tid in &siblings {
                // Perform clear_child_tid wake for the sibling.
                do_clear_child_tid(*sibling_tid);
                // Remove sibling from process table (shared resources stay via Arc).
                {
                    let mut table = crate::process::PROCESS_TABLE.lock();
                    table.reap(*sibling_tid);
                }
                // Mark sibling's scheduler task as Dead.
                crate::task::mark_task_dead_by_pid(*sibling_tid);
            }

            // Clear the members list — only we remain, and we're about to exit.
            {
                let mut members = tg.members.lock();
                members.clear();
            }
        }

        // Now do our own clear_child_tid + full exit.
        do_clear_child_tid(pid);
        do_full_process_exit(pid, code);
    }
    // pid == 0 (kernel context): just die.
    crate::mm::restore_kernel_cr3();
    crate::task::mark_current_dead();
}

// ---------------------------------------------------------------------------
// Phase 14: Signal syscalls (P14-T029, T030, T033)
// ---------------------------------------------------------------------------

/// `kill(pid, sig)` — send a signal to a process (syscall 62).
fn sys_kill(pid: u64, sig: u64) -> u64 {
    let sig = sig as u32;
    let target_pid = pid as i64;

    if sig > 63 {
        return NEG_EINVAL;
    }

    // sig=0: permission check only, no signal sent.
    if sig == 0 {
        let table = crate::process::PROCESS_TABLE.lock();
        return if table.find(pid as crate::process::Pid).is_some() {
            0
        } else {
            NEG_ESRCH
        };
    }

    const NEG_ESRCH_KILL: u64 = (-3_i64) as u64;
    if target_pid > 0 {
        // Send to a specific process (or thread group).
        if send_signal_to_thread_group(target_pid as crate::process::Pid, sig) {
            0
        } else {
            NEG_ESRCH_KILL
        }
    } else if target_pid < -1 {
        // Send to process group |pid|.
        let pgid = (-target_pid) as crate::process::Pid;
        crate::process::send_signal_to_group(pgid, sig);
        0
    } else if target_pid == 0 {
        // Send to caller's process group.
        let caller_pid = crate::process::current_pid();
        let pgid = {
            let table = crate::process::PROCESS_TABLE.lock();
            table.find(caller_pid).map(|p| p.pgid).unwrap_or(0)
        };
        if pgid != 0 {
            crate::process::send_signal_to_group(pgid, sig);
        }
        0
    } else {
        // pid=0 or pid=-1: not fully implemented yet.
        NEG_EINVAL
    }
}

/// Helper: deliver a signal to a thread group (or single-threaded process).
///
/// When the target PID belongs to a thread group, we find any thread in the
/// group that does NOT have the signal blocked and deliver there.  If all
/// threads block the signal, deliver to the group leader (stays pending).
/// For single-threaded processes (no `thread_group`), behaves identically to
/// `send_signal`.
fn send_signal_to_thread_group(pid: crate::process::Pid, sig: u32) -> bool {
    use crate::process::{PROCESS_TABLE, send_signal};

    // First, check if the target exists and whether it has a thread group.
    let thread_group_info: Option<(u32, alloc::vec::Vec<u32>)> = {
        let table = PROCESS_TABLE.lock();
        match table.find(pid) {
            Some(proc) => {
                match &proc.thread_group {
                    None => {
                        // Single-threaded — fall through to normal delivery.
                        None
                    }
                    Some(tg) => {
                        let members = tg.members.lock();
                        Some((tg.leader_tid, members.clone()))
                    }
                }
            }
            None => {
                // Leader may have been reaped — scan for any thread with
                // matching tgid to recover the thread group.
                let mut found = None;
                for p in table.iter() {
                    if p.tgid == pid
                        && let Some(ref tg) = p.thread_group
                    {
                        let members = tg.members.lock();
                        found = Some((tg.leader_tid, members.clone()));
                        break;
                    }
                }
                match found {
                    Some(info) => Some(info),
                    None => return false,
                }
            }
        }
    };

    match thread_group_info {
        None => {
            // Single-threaded process — deliver directly.
            send_signal(pid, sig)
        }
        Some((leader_tid, members)) => {
            // Multi-threaded: find a thread that does not block this signal.
            let sig_mask = 1u64 << sig;
            let table = PROCESS_TABLE.lock();

            // First pass: find any member that doesn't block the signal.
            for &tid in &members {
                if let Some(proc) = table.find(tid)
                    && proc.blocked_signals & sig_mask == 0
                {
                    // Found an unblocked thread — deliver here.
                    drop(table);
                    return send_signal(tid, sig);
                }
            }

            // All threads block the signal — deliver to group leader
            // (it stays pending until someone unblocks).
            drop(table);
            send_signal(leader_tid, sig)
        }
    }
}

/// `tkill(tid, sig)` — send a signal to a specific thread (syscall 200).
fn sys_tkill(tid: u64, sig: u64) -> u64 {
    let sig = sig as u32;
    if sig > 63 {
        return NEG_EINVAL;
    }
    if sig == 0 {
        // Permission/existence check only.
        let table = crate::process::PROCESS_TABLE.lock();
        return if table.find(tid as crate::process::Pid).is_some() {
            0
        } else {
            NEG_ESRCH
        };
    }
    if crate::process::send_signal(tid as crate::process::Pid, sig) {
        0
    } else {
        NEG_ESRCH
    }
}

/// `rt_sigreturn()` — restore interrupted register state from sigframe (syscall 15).
///
/// This is a divergent syscall: it reads the sigframe from the user stack,
/// restores all saved registers and the signal mask, and enters ring 3 at
/// the interrupted instruction.  It never returns through the normal path.
fn sys_sigreturn(user_rsp: u64) -> ! {
    let pid = crate::process::current_pid();

    // Restore registers and signal mask from the sigframe.
    let (regs, saved_mask) = match crate::signal::restore_sigframe(user_rsp) {
        Some(r) => r,
        None => {
            log::warn!(
                "[p{}] sigreturn: invalid sigframe at rsp {:#x}",
                pid,
                user_rsp
            );
            sys_exit(-11); // SIGSEGV
        }
    };

    // Restore the signal mask and clear SS_ONSTACK based on kernel state
    // (not user-provided uc_stack flags, which userspace could corrupt).
    {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(proc) = table.find_mut(pid) {
            proc.blocked_signals = saved_mask & !UNBLOCKABLE_MASK;
            if proc.alt_stack_flags & crate::process::SS_ONSTACK != 0 {
                proc.alt_stack_flags &= !crate::process::SS_ONSTACK;
            }
        }
    }

    // Validate restored RIP and RSP are canonical userspace addresses.
    // A corrupt sigframe could cause iretq to fault in ring 0.
    const USER_ADDR_LIMIT: u64 = 0x0000_8000_0000_0000;
    if regs.rip >= USER_ADDR_LIMIT || regs.rsp >= USER_ADDR_LIMIT {
        log::warn!(
            "[p{}] sigreturn: non-canonical rip={:#x} or rsp={:#x}",
            pid,
            regs.rip,
            regs.rsp,
        );
        sys_exit(-11); // SIGSEGV
    }

    log::debug!(
        "[p{}] sigreturn → rip={:#x} rsp={:#x}",
        pid,
        regs.rip,
        regs.rsp,
    );

    // Restore all registers and enter ring 3 at the interrupted instruction.
    // We use iretq with a full register restore to return to the exact
    // pre-signal state.
    unsafe { restore_and_enter_userspace(&regs) }
}

/// Enter ring 3 with a full set of restored registers from a sigframe.
///
/// Restores all GPRs then uses `iretq` to return to the interrupted
/// instruction with the correct RSP and RFLAGS.
///
/// # Safety
///
/// `regs` must contain valid userspace addresses for RIP and RSP.
unsafe fn restore_and_enter_userspace(regs: &crate::signal::SavedUserRegs) -> ! {
    unsafe {
        use core::arch::asm;
        // We need to restore all GPRs.  The simplest approach: push the iretq
        // frame first, then load all GPRs from the struct, then iretq.
        //
        // We save the struct pointer in a register, set up the iretq frame,
        // then load all registers from the struct.
        let ss = u64::from(crate::arch::x86_64::gdt::user_data_selector().0);
        let cs = u64::from(crate::arch::x86_64::gdt::user_code_selector().0);
        // Sanitize rflags: clear all privileged/reserved bits that could cause
        // #GP during iretq, then force IF (bit 9) and reserved bit 1.
        // Cleared: IOPL (12-13), NT (14), VM (17), VIF (19), VIP (20), ID (21).
        const PRIV_MASK: u64 =
            (1 << 12) | (1 << 13) | (1 << 14) | (1 << 17) | (1 << 19) | (1 << 20) | (1 << 21);
        let rflags = (regs.rflags & !PRIV_MASK) | 0x202;

        asm!(
            // Build the iretq frame on the kernel stack.
            "push {ss}",
            "push {user_rsp}",
            "push {rflags}",
            "push {cs}",
            "push {user_rip}",
            // Now restore all GPRs from the SavedUserRegs struct.
            // r14 holds the pointer to the struct (chosen because we restore it last-ish).
            "mov r15, [r14 + 120]",  // r15 offset
            "mov r13, [r14 + 104]",  // r13
            "mov r12, [r14 + 96]",   // r12
            "mov r11, [r14 + 88]",   // r11
            "mov r10, [r14 + 80]",   // r10
            "mov r9, [r14 + 72]",    // r9
            "mov r8, [r14 + 64]",    // r8
            "mov rbp, [r14 + 48]",   // rbp
            "mov rbx, [r14 + 8]",    // rbx
            "mov rdx, [r14 + 24]",   // rdx
            "mov rsi, [r14 + 32]",   // rsi
            "mov rdi, [r14 + 40]",   // rdi
            "mov rcx, [r14 + 16]",   // rcx
            "mov rax, [r14 + 0]",    // rax
            // Restore r14 last (it was our pointer register).
            "mov r14, [r14 + 112]",  // r14
            "iretq",
            ss       = in(reg) ss,
            user_rsp = in(reg) regs.rsp,
            rflags   = in(reg) rflags,
            cs       = in(reg) cs,
            user_rip = in(reg) regs.rip,
            in("r14") regs as *const crate::signal::SavedUserRegs as u64,
            options(noreturn)
        )
    }
}

/// `rt_sigaction(sig, act, oldact, sigsetsize)` — install/query signal handler (syscall 13).
fn sys_rt_sigaction(sig: u64, act_ptr: u64, oldact_ptr: u64) -> u64 {
    let sig = sig as u32;
    if sig == 0 || sig >= 32 {
        return NEG_EINVAL;
    }
    // SIGKILL and SIGSTOP cannot be caught or ignored.
    if sig == crate::process::SIGKILL || sig == crate::process::SIGSTOP {
        return NEG_EINVAL;
    }

    let pid = crate::process::current_pid();
    let mut table = crate::process::PROCESS_TABLE.lock();
    let proc = match table.find_mut(pid) {
        Some(p) => p,
        None => return NEG_EINVAL,
    };

    // Write old action if requested.
    // Linux struct sigaction layout: sa_handler(8) + sa_flags(8) + sa_restorer(8) + sa_mask(8) = 32 bytes
    if oldact_ptr != 0 {
        let mut old_sa = [0u8; 32];
        match proc.sigaction_get(sig as usize) {
            crate::process::SignalAction::Default => {
                old_sa[0..8].copy_from_slice(&0u64.to_ne_bytes()); // SIG_DFL
            }
            crate::process::SignalAction::Ignore => {
                old_sa[0..8].copy_from_slice(&1u64.to_ne_bytes()); // SIG_IGN
            }
            crate::process::SignalAction::Handler {
                entry,
                mask,
                flags,
                restorer,
            } => {
                old_sa[0..8].copy_from_slice(&entry.to_ne_bytes());
                old_sa[8..16].copy_from_slice(&flags.to_ne_bytes());
                old_sa[16..24].copy_from_slice(&restorer.to_ne_bytes());
                // Convert kernel mask back to userspace (0-indexed).
                old_sa[24..32].copy_from_slice(&(mask >> 1).to_ne_bytes());
            }
        }
        if crate::mm::user_mem::copy_to_user(oldact_ptr, &old_sa).is_err() {
            return NEG_EFAULT;
        }
    }

    // Read new action if provided.
    if act_ptr != 0 {
        let mut sa = [0u8; 32];
        if crate::mm::user_mem::copy_from_user(&mut sa, act_ptr).is_err() {
            return NEG_EFAULT;
        }
        let handler_addr = u64::from_ne_bytes(sa[0..8].try_into().unwrap());
        let sa_flags = u64::from_ne_bytes(sa[8..16].try_into().unwrap());
        let sa_restorer = u64::from_ne_bytes(sa[16..24].try_into().unwrap());

        // Reject handler or restorer pointing into kernel space.
        // Values 0 (SIG_DFL) and 1 (SIG_IGN) are handled by the match below.
        const USER_LIMIT: u64 = 0x0000_8000_0000_0000;
        if handler_addr >= USER_LIMIT {
            return NEG_EINVAL;
        }
        if sa_restorer != 0 && sa_restorer >= USER_LIMIT {
            return NEG_EINVAL;
        }
        // Convert userspace mask (0-indexed) to kernel mask (signal-number-indexed).
        let sa_mask = u64::from_ne_bytes(sa[24..32].try_into().unwrap()) << 1;

        let new_action = match handler_addr {
            0 => crate::process::SignalAction::Default, // SIG_DFL
            1 => crate::process::SignalAction::Ignore,  // SIG_IGN
            _ => {
                // Warn if SA_RESTORER is missing — musl always sets it.
                // Without a restorer, the handler cannot return to sigreturn.
                let effective_restorer = if sa_flags & SA_RESTORER != 0 {
                    sa_restorer
                } else {
                    log::warn!(
                        "[p{}] rt_sigaction: sig={} handler {:#x} missing SA_RESTORER",
                        pid,
                        sig,
                        handler_addr,
                    );
                    0 // will fault on handler return, making the bug visible
                };
                crate::process::SignalAction::Handler {
                    entry: handler_addr,
                    mask: sa_mask,
                    flags: sa_flags,
                    restorer: effective_restorer,
                }
            }
        };
        proc.sigaction_set(sig as usize, new_action);
    }

    0
}

/// Signal mask operation constants (Linux).
const SIG_BLOCK: u64 = 0;
const SIG_UNBLOCK: u64 = 1;
const SIG_SETMASK: u64 = 2;

/// Bits that must never be set in blocked_signals (SIGKILL=9, SIGSTOP=19).
const UNBLOCKABLE_MASK: u64 = (1u64 << crate::process::SIGKILL) | (1u64 << crate::process::SIGSTOP);

/// Signal action flags (from Linux uapi).
const SA_RESTORER: u64 = 0x0400_0000;
const SA_ONSTACK: u64 = 0x0800_0000;
#[allow(dead_code)]
const SA_SIGINFO: u64 = 0x0000_0004;
#[allow(dead_code)]
const SA_NODEFER: u64 = 0x4000_0000;
#[allow(dead_code)]
const SA_RESETHAND: u64 = 0x8000_0000;

/// `rt_sigprocmask(how, set_ptr, oldset_ptr, sigsetsize)` — syscall 14.
///
/// Reads/modifies the calling process's blocked-signal mask.
fn sys_rt_sigprocmask(how: u64, set_ptr: u64, oldset_ptr: u64) -> u64 {
    let pid = crate::process::current_pid();

    let mut table = crate::process::PROCESS_TABLE.lock();
    let proc = match table.find_mut(pid) {
        Some(p) => p,
        None => return NEG_EINVAL,
    };

    // Write old mask to userspace if requested.
    // Userspace (musl) uses 0-indexed bits: bit N represents signal N+1.
    // Kernel uses signal-number-indexed bits: bit N represents signal N.
    // Convert kernel→userspace by shifting right 1.
    if oldset_ptr != 0 {
        let old_user = proc.blocked_signals >> 1;
        let old_bytes = old_user.to_ne_bytes();
        if crate::mm::user_mem::copy_to_user(oldset_ptr, &old_bytes).is_err() {
            return NEG_EFAULT;
        }
    }

    // Apply new mask if set_ptr is non-null.
    if set_ptr != 0 {
        let mut set_bytes = [0u8; 8];
        if crate::mm::user_mem::copy_from_user(&mut set_bytes, set_ptr).is_err() {
            return NEG_EFAULT;
        }
        // Convert userspace→kernel by shifting left 1.
        let set = u64::from_ne_bytes(set_bytes) << 1;

        match how {
            SIG_BLOCK => proc.blocked_signals |= set,
            SIG_UNBLOCK => proc.blocked_signals &= !set,
            SIG_SETMASK => proc.blocked_signals = set,
            _ => return NEG_EINVAL,
        }

        // SIGKILL and SIGSTOP can never be blocked.
        proc.blocked_signals &= !UNBLOCKABLE_MASK;
    }

    // Drop the lock before checking pending signals so we don't deadlock.
    // Check pending signals after any operation that could unblock signals.
    let needs_check = set_ptr != 0 && (how == SIG_UNBLOCK || how == SIG_SETMASK);
    drop(table);

    // After SIG_UNBLOCK, deliver any newly-unblocked pending signals immediately.
    // Pass 0 as the syscall result since rt_sigprocmask succeeds.
    if needs_check {
        check_pending_signals(0);
    }

    0
}

// ---------------------------------------------------------------------------
// Phase 19: sigaltstack (P19-T020, T021)
// ---------------------------------------------------------------------------

/// `sigaltstack(ss, old_ss)` — register/query alternate signal stack (syscall 131).
fn sys_sigaltstack(ss_ptr: u64, old_ss_ptr: u64) -> u64 {
    let pid = crate::process::current_pid();

    let mut table = crate::process::PROCESS_TABLE.lock();
    let proc = match table.find_mut(pid) {
        Some(p) => p,
        None => return NEG_EINVAL,
    };

    // Write current alt stack to old_ss_ptr if requested.
    if old_ss_ptr != 0 {
        // struct stack_t: ss_sp(8) + ss_flags(4) + pad(4) + ss_size(8) = 24 bytes
        let mut buf = [0u8; 24];
        buf[0..8].copy_from_slice(&proc.alt_stack_base.to_ne_bytes());
        buf[8..12].copy_from_slice(&proc.alt_stack_flags.to_ne_bytes());
        buf[16..24].copy_from_slice(&proc.alt_stack_size.to_ne_bytes());
        if crate::mm::user_mem::copy_to_user(old_ss_ptr, &buf).is_err() {
            return NEG_EFAULT;
        }
    }

    // Read and set new alt stack if provided.
    if ss_ptr != 0 {
        // Cannot change alt stack while executing on it.
        if proc.alt_stack_flags & crate::process::SS_ONSTACK != 0 {
            return NEG_EPERM;
        }

        let mut buf = [0u8; 24];
        if crate::mm::user_mem::copy_from_user(&mut buf, ss_ptr).is_err() {
            return NEG_EFAULT;
        }
        let ss_sp = u64::from_ne_bytes(buf[0..8].try_into().unwrap());
        let ss_flags = u32::from_ne_bytes(buf[8..12].try_into().unwrap());
        let ss_size = u64::from_ne_bytes(buf[16..24].try_into().unwrap());

        if ss_flags & crate::process::SS_DISABLE != 0 {
            // Disable the alt stack.
            proc.alt_stack_base = 0;
            proc.alt_stack_size = 0;
            proc.alt_stack_flags = crate::process::SS_DISABLE;
        } else {
            // Only SS_DISABLE is accepted from userspace; SS_ONSTACK is a
            // read-only status flag maintained by the kernel.
            if ss_flags & !crate::process::SS_DISABLE != 0 {
                return NEG_EINVAL;
            }
            // Validate minimum size.
            if ss_size < crate::process::MINSIGSTKSZ {
                return NEG_EINVAL;
            }
            // Validate range is within canonical userspace (above null page).
            const USER_LIMIT: u64 = 0x0000_8000_0000_0000;
            if !(0x1000..USER_LIMIT).contains(&ss_sp)
                || ss_sp
                    .checked_add(ss_size)
                    .is_none_or(|top| top > USER_LIMIT)
            {
                return NEG_EINVAL;
            }
            proc.alt_stack_base = ss_sp;
            proc.alt_stack_size = ss_size;
            proc.alt_stack_flags = 0;
        }
    }

    0
}

// ---------------------------------------------------------------------------
// Phase 14: pipe (P14-T009) and dup2 (P14-T014)
// ---------------------------------------------------------------------------

/// `pipe(pipefd_ptr)` — create a pipe (syscall 22).
///
/// Writes `[read_fd, write_fd]` to userspace memory at `pipefd_ptr`.
fn sys_pipe_with_flags(pipefd_ptr: u64, cloexec: bool) -> u64 {
    // Pipe starts with reader_count=0, writer_count=0.
    // We bump refcounts explicitly after each successful FD allocation.
    let pipe_id = crate::pipe::create_pipe();

    let read_entry = FdEntry {
        backend: FdBackend::PipeRead { pipe_id },
        offset: 0,
        readable: true,
        writable: false,
        cloexec,
        nonblock: false,
    };
    let write_entry = FdEntry {
        backend: FdBackend::PipeWrite { pipe_id },
        offset: 0,
        readable: false,
        writable: true,
        cloexec,
        nonblock: false,
    };

    let read_fd = match alloc_fd(3, read_entry) {
        Some(fd) => fd,
        None => {
            // No FDs reference this pipe yet — free the slot directly.
            crate::pipe::free_pipe(pipe_id);
            return NEG_EMFILE;
        }
    };
    crate::pipe::pipe_add_reader(pipe_id); // reader_count: 0 → 1

    let write_fd = match alloc_fd(3, write_entry) {
        Some(fd) => fd,
        None => {
            // Only the read FD exists — close it properly.
            with_current_fd_mut(read_fd, |slot| *slot = None);
            crate::pipe::pipe_close_reader(pipe_id); // reader_count: 1 → 0
            // writer_count is still 0, so pipe slot is now freed.
            return NEG_EMFILE;
        }
    };
    crate::pipe::pipe_add_writer(pipe_id); // writer_count: 0 → 1

    // Write [read_fd, write_fd] as two i32s to user memory.
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&(read_fd as i32).to_ne_bytes());
    bytes[4..].copy_from_slice(&(write_fd as i32).to_ne_bytes());
    if crate::mm::user_mem::copy_to_user(pipefd_ptr, &bytes).is_err() {
        // Both FDs exist — close them properly via refcounts.
        with_current_fd_mut(read_fd, |slot| *slot = None);
        with_current_fd_mut(write_fd, |slot| *slot = None);
        crate::pipe::pipe_close_reader(pipe_id);
        crate::pipe::pipe_close_writer(pipe_id);
        return NEG_EFAULT;
    }

    log::info!(
        "[pipe] created pipe_id={} → fd[{}(r), {}(w)]",
        pipe_id,
        read_fd,
        write_fd
    );
    0
}

/// `dup2(oldfd, newfd)` — duplicate a file descriptor (syscall 33).
fn sys_dup(oldfd: u64) -> u64 {
    let oldfd = oldfd as usize;
    if oldfd >= MAX_FDS {
        return NEG_EBADF;
    }

    let entry = match current_fd_entry(oldfd) {
        Some(e) => e,
        None => return NEG_EBADF,
    };

    // Remember backend info so we only bump refcount on successful alloc.
    let backend_clone = entry.backend.clone();

    // POSIX: dup always clears FD_CLOEXEC on the new descriptor.
    let mut entry_copy = entry;
    entry_copy.cloexec = false;

    match alloc_fd(0, entry_copy) {
        Some(newfd) => {
            // Increment refcount only after successful allocation.
            match &backend_clone {
                FdBackend::PipeRead { pipe_id } => crate::pipe::pipe_add_reader(*pipe_id),
                FdBackend::PipeWrite { pipe_id } => crate::pipe::pipe_add_writer(*pipe_id),
                FdBackend::PtyMaster { pty_id } => crate::pty::add_master_ref(*pty_id),
                FdBackend::PtySlave { pty_id } => crate::pty::add_slave_ref(*pty_id),
                FdBackend::Socket { handle } => crate::net::add_socket_ref(*handle),
                FdBackend::UnixSocket { handle } => crate::net::unix::add_unix_socket_ref(*handle),
                FdBackend::Epoll { instance_id } => epoll_add_ref(*instance_id),
                _ => {}
            }
            log::info!("[dup] fd {} → fd {}", oldfd, newfd);
            newfd as u64
        }
        None => NEG_EMFILE,
    }
}

fn sys_dup2(oldfd: u64, newfd: u64) -> u64 {
    let oldfd = oldfd as usize;
    let newfd = newfd as usize;

    if oldfd >= MAX_FDS || newfd >= MAX_FDS {
        return NEG_EBADF;
    }

    // dup2(fd, fd) returns fd without closing.
    if oldfd == newfd {
        return if current_fd_entry(oldfd).is_some() {
            newfd as u64
        } else {
            NEG_EBADF
        };
    }

    let entry = match current_fd_entry(oldfd) {
        Some(e) => e,
        None => return NEG_EBADF,
    };

    // Close newfd if it's open (including pipe cleanup).
    if current_fd_entry(newfd).is_some() {
        sys_linux_close(newfd as u64);
    }

    // Increment refcount for the duplicated FD.
    match &entry.backend {
        FdBackend::PipeRead { pipe_id } => crate::pipe::pipe_add_reader(*pipe_id),
        FdBackend::PipeWrite { pipe_id } => crate::pipe::pipe_add_writer(*pipe_id),
        FdBackend::PtyMaster { pty_id } => crate::pty::add_master_ref(*pty_id),
        FdBackend::PtySlave { pty_id } => crate::pty::add_slave_ref(*pty_id),
        FdBackend::Socket { handle } => crate::net::add_socket_ref(*handle),
        FdBackend::UnixSocket { handle } => crate::net::unix::add_unix_socket_ref(*handle),
        FdBackend::Epoll { instance_id } => epoll_add_ref(*instance_id),
        _ => {}
    }

    // Copy the FD entry to the new slot.
    // POSIX: dup2 always clears FD_CLOEXEC on the new descriptor.
    let mut entry_copy = entry;
    entry_copy.cloexec = false;
    with_current_fd_mut(newfd, |slot| {
        *slot = Some(entry_copy);
    });

    newfd as u64
}

// ---------------------------------------------------------------------------
// Phase 14: process group syscalls (P14-T035)
// ---------------------------------------------------------------------------

/// `setpgid(pid, pgid)` — set process group ID (syscall 109).
fn sys_setpgid(pid: u64, pgid: u64) -> u64 {
    let caller = crate::process::current_pid();
    let target = if pid == 0 {
        caller
    } else {
        pid as crate::process::Pid
    };
    let new_pgid = if pgid == 0 {
        target
    } else {
        pgid as crate::process::Pid
    };

    let mut table = crate::process::PROCESS_TABLE.lock();
    match table.find_mut(target) {
        Some(p) => {
            p.pgid = new_pgid;
            0
        }
        None => NEG_EINVAL,
    }
}

/// `getpgid(pid)` — get process group ID (syscall 121).
fn sys_getpgid(pid: u64) -> u64 {
    let target = if pid == 0 {
        crate::process::current_pid()
    } else {
        pid as crate::process::Pid
    };

    let table = crate::process::PROCESS_TABLE.lock();
    match table.find(target) {
        Some(p) => p.pgid as u64,
        None => NEG_EINVAL,
    }
}

/// `setsid()` — create a new session (syscall 112).
fn sys_setsid() -> u64 {
    let calling_pid = crate::process::current_pid();
    let mut table = crate::process::PROCESS_TABLE.lock();

    // POSIX: fail if the caller is already a process-group leader (pgid == pid).
    if let Some(proc) = table.find(calling_pid) {
        if proc.pgid == calling_pid {
            return NEG_EPERM;
        }
    } else {
        return NEG_ESRCH;
    }

    if let Some(proc) = table.find_mut(calling_pid) {
        proc.session_id = calling_pid;
        proc.pgid = calling_pid;
        proc.controlling_tty = None;
    }
    calling_pid as u64
}

/// `getsid(pid)` — get session ID (syscall 124).
fn sys_getsid(pid: u64) -> u64 {
    let target = if pid == 0 {
        crate::process::current_pid()
    } else {
        pid as crate::process::Pid
    };
    let table = crate::process::PROCESS_TABLE.lock();
    match table.find(target) {
        Some(p) => p.session_id as u64,
        None => NEG_ESRCH,
    }
}

/// `nanosleep(req, rem)` — sleep for the specified time (syscall 35).
///
/// Reads a `timespec` struct from user memory and yield-loops for the
/// requested number of timer ticks.
fn sys_nanosleep(req_ptr: u64) -> u64 {
    if req_ptr == 0 {
        return NEG_EFAULT;
    }
    let mut ts = [0u8; 16]; // struct timespec { tv_sec: i64, tv_nsec: i64 }
    if crate::mm::user_mem::copy_from_user(&mut ts, req_ptr).is_err() {
        return NEG_EFAULT;
    }
    let secs = i64::from_ne_bytes(ts[0..8].try_into().unwrap());
    let nsecs = i64::from_ne_bytes(ts[8..16].try_into().unwrap());
    if secs < 0 || !(0..1_000_000_000).contains(&nsecs) {
        return NEG_EINVAL;
    }
    // Each PIT tick is ~10ms (100 Hz). Convert seconds+nsec to ticks.
    let ticks = (secs as u64).saturating_mul(100) + (nsecs as u64) / 10_000_000;
    let start = crate::arch::x86_64::interrupts::tick_count();
    let pid = crate::process::current_pid();
    let saved_user_rsp = per_core_syscall_user_rsp();
    while crate::arch::x86_64::interrupts::tick_count().wrapping_sub(start) < ticks {
        crate::task::yield_now();
        restore_caller_context(pid, saved_user_rsp);
        if has_pending_signal() {
            return NEG_EINTR;
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Phase 27: User/group identity syscalls
// ---------------------------------------------------------------------------

/// Helper: get the uid/gid/euid/egid of the current process.
fn current_process_ids() -> (u32, u32, u32, u32) {
    let pid = crate::process::current_pid();
    let table = crate::process::PROCESS_TABLE.lock();
    match table.find(pid) {
        Some(p) => (p.uid, p.gid, p.euid, p.egid),
        None => (0, 0, 0, 0),
    }
}

/// `times(buf)` — fill struct tms with CPU time accounting (syscall 100).
///
/// struct tms layout (Linux compatible, 4 x i64):
///   offset 0: tms_utime  — user CPU time
///   offset 8: tms_stime  — system CPU time
///   offset 16: tms_cutime — children user CPU time
///   offset 24: tms_cstime — children system CPU time
/// Returns: clock ticks since boot.
fn sys_times(buf_ptr: u64) -> u64 {
    let (user_ticks, system_ticks) = crate::task::scheduler::current_task_times().unwrap_or((0, 0));
    if buf_ptr != 0 {
        let mut bytes = [0u8; 32]; // 4 × i64
        bytes[0..8].copy_from_slice(&(user_ticks as i64).to_ne_bytes()); // tms_utime
        bytes[8..16].copy_from_slice(&(system_ticks as i64).to_ne_bytes()); // tms_stime
        bytes[16..24].copy_from_slice(&0_i64.to_ne_bytes()); // tms_cutime (children — not tracked yet)
        bytes[24..32].copy_from_slice(&0_i64.to_ne_bytes()); // tms_cstime
        if crate::mm::user_mem::copy_to_user(buf_ptr, &bytes).is_err() {
            return NEG_EFAULT;
        }
    }
    crate::arch::x86_64::interrupts::tick_count()
}

/// `getuid()` — return real user ID (syscall 102).
fn sys_linux_getuid() -> u64 {
    current_process_ids().0 as u64
}

/// `getgid()` — return real group ID (syscall 104).
fn sys_linux_getgid() -> u64 {
    current_process_ids().1 as u64
}

/// `geteuid()` — return effective user ID (syscall 107).
fn sys_linux_geteuid() -> u64 {
    current_process_ids().2 as u64
}

/// `getegid()` — return effective group ID (syscall 108).
fn sys_linux_getegid() -> u64 {
    current_process_ids().3 as u64
}

/// `setuid(uid)` — set user ID (syscall 105).
///
/// Sets both real uid and effective uid unconditionally.
/// Note: without setuid-bit support, password-authenticated programs
/// like `su` and `login` rely on this being unrestricted. The password
/// check in userspace provides the security boundary.
fn sys_linux_setuid(uid_arg: u64) -> u64 {
    let new_uid = uid_arg as u32;
    let pid = crate::process::current_pid();
    let mut table = crate::process::PROCESS_TABLE.lock();
    let proc = match table.find_mut(pid) {
        Some(p) => p,
        None => return NEG_EPERM,
    };
    proc.uid = new_uid;
    proc.euid = new_uid;
    0
}

/// `setgid(gid)` — set group ID (syscall 106).
///
/// Unconditional — see `sys_linux_setuid` comment.
fn sys_linux_setgid(gid_arg: u64) -> u64 {
    let new_gid = gid_arg as u32;
    let pid = crate::process::current_pid();
    let mut table = crate::process::PROCESS_TABLE.lock();
    let proc = match table.find_mut(pid) {
        Some(p) => p,
        None => return NEG_EPERM,
    };
    proc.gid = new_gid;
    proc.egid = new_gid;
    0
}

/// `setreuid(ruid, euid)` — set real and effective user IDs (syscall 113).
///
/// If ruid != -1: set real uid (only if euid==0 or ruid matches current real/effective uid).
/// If euid != -1: set effective uid (only if euid==0 or value matches current real uid).
fn sys_linux_setreuid(ruid_arg: u64, euid_arg: u64) -> u64 {
    let ruid = ruid_arg as i32;
    let euid = euid_arg as i32;
    let pid = crate::process::current_pid();
    let mut table = crate::process::PROCESS_TABLE.lock();
    let proc = match table.find_mut(pid) {
        Some(p) => p,
        None => return NEG_EPERM,
    };

    if ruid != -1 {
        let new_ruid = ruid as u32;
        if proc.euid == 0 || new_ruid == proc.uid || new_ruid == proc.euid {
            proc.uid = new_ruid;
        } else {
            return NEG_EPERM;
        }
    }

    if euid != -1 {
        let new_euid = euid as u32;
        if proc.euid == 0 || new_euid == proc.uid || new_euid == proc.euid {
            proc.euid = new_euid;
        } else {
            return NEG_EPERM;
        }
    }

    0
}

/// `setregid(rgid, egid)` — set real and effective group IDs (syscall 114).
fn sys_linux_setregid(rgid_arg: u64, egid_arg: u64) -> u64 {
    let rgid = rgid_arg as i32;
    let egid = egid_arg as i32;
    let pid = crate::process::current_pid();
    let mut table = crate::process::PROCESS_TABLE.lock();
    let proc = match table.find_mut(pid) {
        Some(p) => p,
        None => return NEG_EPERM,
    };

    if rgid != -1 {
        let new_rgid = rgid as u32;
        if proc.egid == 0 || new_rgid == proc.gid || new_rgid == proc.egid {
            proc.gid = new_rgid;
        } else {
            return NEG_EPERM;
        }
    }

    if egid != -1 {
        let new_egid = egid as u32;
        if proc.egid == 0 || new_egid == proc.gid || new_egid == proc.egid {
            proc.egid = new_egid;
        } else {
            return NEG_EPERM;
        }
    }

    0
}

/// `fork()` — create a child process that resumes after the syscall with rax=0.
///
/// Allocates a fresh page table for the child (eager copy of user pages),
/// registers the child in the process table, and spawns a kernel task whose
/// entry function enters ring 3 at `user_rip` with `user_rsp` and rax=0.
///
/// Returns the child PID to the parent.
fn sys_fork(user_rip: u64, user_rsp: u64) -> u64 {
    let parent_pid = crate::process::current_pid();
    log::info!("[p{}] fork()", parent_pid);

    // Allocate a new page table for the child, copying kernel entries.
    let child_cr3 = match crate::mm::new_process_page_table() {
        Some(f) => f,
        None => {
            log::warn!("[fork] out of frames for child page table");
            return u64::MAX;
        }
    };

    // CoW-clone user-accessible pages: share physical frames between parent
    // and child, clearing WRITABLE so writes trigger page faults.
    let phys_off = crate::mm::phys_offset();
    let cow_result = {
        // SAFETY: child_cr3 was just allocated; no other mapper over it exists.
        let mut child_mapper = unsafe { crate::mm::mapper_for_frame(child_cr3) };
        // SAFETY: current CR3 is the parent; we modify its PTEs to clear WRITABLE.
        unsafe { cow_clone_user_pages(phys_off, &mut child_mapper) }
        // child_mapper drops here, ending its borrow of the page table.
    };
    if let Err(e) = cow_result {
        log::warn!("[fork] CoW clone failed: {:?}", e);
        crate::mm::free_process_page_table(child_cr3.start_address().as_u64());
        return u64::MAX;
    }

    // Inherit parent's brk/mmap state and FD table so the child's heap
    // and file descriptors are consistent with the copied address space.
    let (
        parent_brk,
        parent_mmap,
        parent_fds,
        parent_pgid,
        parent_cwd,
        parent_blocked_signals,
        parent_signal_actions,
        parent_alt_stack,
        parent_fs_base,
        parent_ids,
        parent_umask,
        parent_session_id,
        parent_ctty,
        parent_mappings,
        parent_exec_path,
        parent_cmdline,
    ) = {
        let table = crate::process::PROCESS_TABLE.lock();
        match table.find(parent_pid) {
            Some(p) => (
                p.brk_current,
                p.mmap_next,
                p.fd_table_snapshot(),
                p.pgid,
                p.cwd.clone(),
                p.blocked_signals,
                p.signal_actions_snapshot(),
                (p.alt_stack_base, p.alt_stack_size, p.alt_stack_flags),
                p.fs_base,
                (p.uid, p.gid, p.euid, p.egid),
                p.umask,
                p.session_id,
                p.controlling_tty.clone(),
                p.mappings.clone(),
                p.exec_path.clone(),
                p.cmdline.clone(),
            ),
            None => (
                0,
                0,
                {
                    const NONE: Option<crate::process::FdEntry> = None;
                    [NONE; crate::process::MAX_FDS]
                },
                0,
                alloc::string::String::from("/"),
                0,
                [crate::process::SignalAction::Default; 32],
                (0u64, 0u64, 0u32),
                0,
                (0u32, 0u32, 0u32, 0u32),
                0o022,
                0,
                Some(crate::process::ControllingTty::Console),
                alloc::vec::Vec::new(),
                alloc::string::String::new(),
                alloc::vec::Vec::new(),
            ),
        }
    };

    // Increment refcounts (pipes + PTYs) for cloned FDs before creating the child.
    crate::process::add_fd_refs(&parent_fds);

    // Create child process entry with cloned FD table (Phase 14, P14-T003).
    // Inherit parent's pgid so fork children are in the same process group.
    let child_pid = crate::process::spawn_process_with_cr3_and_fds(
        parent_pid,
        user_rip,
        user_rsp,
        x86_64::PhysAddr::new(child_cr3.start_address().as_u64()),
        parent_brk,
        parent_mmap,
        parent_fds,
        parent_pgid,
    );

    // Inherit parent's cwd, signal mask, and signal actions in the child.
    {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(child) = table.find_mut(child_pid) {
            child.cwd = parent_cwd;
            child.blocked_signals = parent_blocked_signals;
            child.signal_actions = parent_signal_actions;
            child.alt_stack_base = parent_alt_stack.0;
            child.alt_stack_size = parent_alt_stack.1;
            child.alt_stack_flags = parent_alt_stack.2;
            child.fs_base = parent_fs_base;
            child.uid = parent_ids.0;
            child.gid = parent_ids.1;
            child.euid = parent_ids.2;
            child.egid = parent_ids.3;
            child.umask = parent_umask;
            child.session_id = parent_session_id;
            child.controlling_tty = parent_ctty;
            child.mappings = parent_mappings;
            child.exec_path = parent_exec_path;
            child.cmdline = parent_cmdline;
        }
    }

    crate::task::spawn_fork_task(
        crate::process::make_fork_ctx(child_pid, user_rip, user_rsp),
        "fork-child",
    );

    log::info!("[p{}] fork() → child pid {}", parent_pid, child_pid);
    child_pid as u64
}

/// Read a null-terminated array of char* pointers from user memory, copying
/// each pointed-to C string into a kernel `Vec<Vec<u8>>`.
///
/// Returns an empty vec if `array_ptr` is 0 (NULL).
/// Returns at most `max_entries` strings; each string is capped at 4096 bytes.
/// Read a null-terminated array of `char*` pointers from user memory.
///
/// Returns `Ok(vec)` on success, `Err(())` if a user pointer is invalid
/// (caller should return EFAULT).
fn read_user_string_array(
    array_ptr: u64,
    max_entries: usize,
) -> Result<alloc::vec::Vec<alloc::vec::Vec<u8>>, ()> {
    let mut result = alloc::vec::Vec::new();
    if array_ptr == 0 {
        return Ok(result);
    }
    for i in 0..max_entries {
        let ptr_addr = match array_ptr.checked_add((i * 8) as u64) {
            Some(a) => a,
            None => return Err(()),
        };
        let mut ptr_bytes = [0u8; 8];
        if crate::mm::user_mem::copy_from_user(&mut ptr_bytes, ptr_addr).is_err() {
            return Err(());
        }
        let str_ptr = u64::from_ne_bytes(ptr_bytes);
        if str_ptr == 0 {
            break; // NULL terminator
        }
        // Read the C string byte by byte.
        let mut s = alloc::vec::Vec::new();
        let mut found_nul = false;
        for j in 0..4096u64 {
            let addr = match str_ptr.checked_add(j) {
                Some(a) => a,
                None => return Err(()),
            };
            let mut b = [0u8; 1];
            if crate::mm::user_mem::copy_from_user(&mut b, addr).is_err() {
                return Err(());
            }
            if b[0] == 0 {
                found_nul = true;
                break;
            }
            s.push(b[0]);
        }
        if !found_nul {
            return Err(());
        }
        result.push(s);
    }
    Ok(result)
}

/// `execve(filename, argv, envp)` — replace the calling process's image
/// with a new ELF binary read from the ramdisk.
///
/// Phase 14: now parses argv and envp from user memory (Linux ABI).
fn sys_execve(path_ptr: u64, argv_ptr: u64, envp_ptr: u64) -> u64 {
    // Read the filename as a null-terminated C string.
    let mut name_cstr = [0u8; 512];
    let raw_name = match read_user_cstr(path_ptr, &mut name_cstr) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    // Parse argv and envp from user memory.
    let user_argv = match read_user_string_array(argv_ptr, 256) {
        Ok(v) => v,
        Err(()) => return NEG_EFAULT,
    };
    let user_envp = match read_user_string_array(envp_ptr, 256) {
        Ok(v) => v,
        Err(()) => return NEG_EFAULT,
    };

    let (resolved_name, exec_owned, exec_static) = {
        let _mount_guard = MOUNT_OP_LOCK.lock();

        // Follow the final symlink like Linux execve().
        let lexical = match resolve_path_from_dirfd(AT_FDCWD, raw_name) {
            Ok(path) => path,
            Err(err) => return err,
        };
        let resolved = match resolve_existing_fs_path(&lexical, true) {
            Ok(path) => path,
            Err(err) => return err,
        };

        // Phase 27: Execute permission check.
        if let Some((fu, fg, fm)) = path_metadata(&resolved) {
            let (_, _, euid, egid) = current_process_ids();
            if !check_permission(fu, fg, fm, euid, egid, 1) {
                return NEG_EACCES;
            }
        }

        match crate::fs::ramdisk::get_file(&resolved) {
            Some(data) => (resolved, None, Some(data)),
            None => {
                // Phase 31: try ext2, FAT32, and tmpfs before giving up.
                match read_file_from_disk(&resolved) {
                    Ok(buf) => (resolved, Some(buf), None),
                    Err(errno) => {
                        log::warn!("[execve] file not found or rejected: {}", resolved);
                        return errno;
                    }
                }
            }
        }
    };
    let name: &str = &resolved_name;
    log::info!("[p{}] execve({})", crate::process::current_pid(), name);
    let data: &[u8] = match (exec_static, exec_owned.as_deref()) {
        (Some(data), None) => data,
        (None, Some(data)) => data,
        _ => return NEG_EIO,
    };

    // Allocate a fresh page table for the new image.
    const NEG_ENOMEM: u64 = (-12_i64) as u64;
    let new_cr3 = match crate::mm::new_process_page_table() {
        Some(f) => f,
        None => return NEG_ENOMEM,
    };

    let phys_off = crate::mm::phys_offset();

    // Build argv slices: use user-provided argv if non-empty, else [filename].
    let argv_refs: alloc::vec::Vec<&[u8]> = if user_argv.is_empty() {
        alloc::vec![name.as_bytes()]
    } else {
        user_argv.iter().map(|v| v.as_slice()).collect()
    };
    let envp_refs: alloc::vec::Vec<&[u8]> = user_envp.iter().map(|v| v.as_slice()).collect();

    let (loaded, user_rsp) = {
        // SAFETY: new_cr3 is freshly allocated; no other mapper exists.
        let mut mapper = unsafe { crate::mm::mapper_for_frame(new_cr3) };
        let loaded = match unsafe { crate::mm::elf::load_elf_into(&mut mapper, phys_off, data) } {
            Ok(l) => l,
            Err(e) => {
                log::warn!("[execve] ELF load failed: {:?}", e);
                return NEG_ENOENT; // treat invalid ELF as "not found"
            }
        };
        // SAFETY: stack pages were just mapped by load_elf_into; mapper is valid.
        let user_rsp = match unsafe {
            crate::mm::elf::setup_abi_stack_with_envp(
                loaded.stack_top,
                &mapper,
                phys_off,
                &argv_refs,
                &envp_refs,
                loaded.phdr_vaddr,
                loaded.phnum,
            )
        } {
            Ok(rsp) => rsp,
            Err(e) => {
                log::warn!("[execve] ABI stack setup failed: {:?}", e);
                return NEG_ENOMEM;
            }
        };
        (loaded, user_rsp)
    };

    // Close file descriptors with FD_CLOEXEC set.
    let pid = crate::process::current_pid();
    crate::process::close_cloexec_fds(pid);

    // Update the process entry with the new CR3 and entry point.
    // Reset brk/mmap state since the address space is completely replaced.
    {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(proc) = table.find_mut(pid) {
            proc.page_table_root = Some(x86_64::PhysAddr::new(new_cr3.start_address().as_u64()));
            proc.entry_point = loaded.entry;
            proc.user_stack_top = user_rsp;
            proc.brk_current = 0;
            proc.mmap_next = 0;
            proc.mappings.clear(); // Phase 36: clear stale VMAs from old address space.
            proc.exec_path = alloc::string::String::from(name);
            proc.cmdline = if user_argv.is_empty() {
                alloc::vec![alloc::string::String::from(name)]
            } else {
                user_argv
                    .iter()
                    .filter_map(|arg| core::str::from_utf8(arg).ok())
                    .map(alloc::string::String::from)
                    .collect()
            };
        }
    }

    // Switch to the new page table and enter ring 3.
    // SAFETY: new_cr3 is valid, entry and user_rsp are within it.
    unsafe {
        use x86_64::registers::control::{Cr3, Cr3Flags};
        // Capture old CR3 before switching so we can free its frames after.
        let (old_cr3, _) = Cr3::read();
        let old_cr3_phys = old_cr3.start_address().as_u64();
        Cr3::write(new_cr3, Cr3Flags::empty());
        // Free the old page table's user-space frames now that CR3 no longer
        // points to it. The bump allocator makes this a no-op today; the
        // real reclamation happens in Phase 13 when a free list is added.
        crate::mm::free_process_page_table(old_cr3_phys);
        // Update TSS.RSP0 so interrupts from ring 3 use the correct kernel stack.
        let kstack_top = crate::process::PROCESS_TABLE
            .lock()
            .find(pid)
            .map(|p| p.kernel_stack_top)
            .unwrap_or(0);
        if kstack_top != 0 {
            crate::smp::set_current_core_kernel_stack(kstack_top);
            set_per_core_syscall_stack_top(kstack_top);
        }
        crate::arch::x86_64::enter_userspace(loaded.entry, user_rsp)
    }
}

/// `waitpid(pid, status_ptr, _flags)` — wait for a child to exit.
///
/// Spins with `yield_now()` until the target child is a zombie, then
/// collects its exit code and reaps it.
/// `waitpid(pid, status_ptr, options)` — wait for a child to exit or stop.
///
/// Supports pid > 0 (specific child), pid == -1 (any child), pid == 0
/// (any child in caller's process group).
/// WUNTRACED (0x2): also report stopped children.
fn sys_waitpid(pid: u64, status_ptr: u64, options: u64) -> u64 {
    let target_pid = pid as i64;
    let calling_pid = crate::process::current_pid();
    let saved_user_rsp = per_core_syscall_user_rsp();
    const WNOHANG: u64 = 0x1;
    const WUNTRACED: u64 = 0x2;
    let report_stopped = options & WUNTRACED != 0;

    // For specific PID: verify it's a child.
    if target_pid > 0 {
        let table = crate::process::PROCESS_TABLE.lock();
        const NEG_ECHILD_PRE: u64 = (-10_i64) as u64;
        match table.find(target_pid as crate::process::Pid) {
            None => return NEG_ECHILD_PRE,
            Some(p) if p.ppid != calling_pid => return NEG_ECHILD_PRE,
            Some(_) => {}
        }
    }

    const NEG_ECHILD: u64 = (-10_i64) as u64;

    loop {
        // Scan for a matching child that is zombie (or stopped if WUNTRACED).
        let result = {
            let mut table = crate::process::PROCESS_TABLE.lock();
            let mut found_pid = None;
            let mut found_code = None;
            let mut found_stopped = false;
            let mut has_eligible_child = false;

            for proc in table.iter() {
                if proc.ppid != calling_pid {
                    continue;
                }
                let matches = match target_pid {
                    p if p > 0 => proc.pid == p as crate::process::Pid,
                    -1 => true, // any child
                    0 => {
                        // Same process group as caller.
                        let caller_pgid = table
                            .find(calling_pid)
                            .map(|p| p.pgid)
                            .unwrap_or(calling_pid);
                        proc.pgid == caller_pgid
                    }
                    neg => proc.pgid == (-neg) as crate::process::Pid,
                };
                if !matches {
                    continue;
                }
                has_eligible_child = true;

                if proc.state == crate::process::ProcessState::Zombie {
                    found_pid = Some(proc.pid);
                    found_code = proc.exit_code;
                    break;
                }
                if report_stopped
                    && proc.state == crate::process::ProcessState::Stopped
                    && !proc.stop_reported
                {
                    found_pid = Some(proc.pid);
                    found_stopped = true;
                    found_code = Some(proc.stop_signal as i32);
                    break;
                }
            }

            if !has_eligible_child {
                return NEG_ECHILD;
            }

            if let Some(pid) = found_pid {
                if found_stopped {
                    // Mark as reported so subsequent waitpid calls don't re-report.
                    if let Some(p) = table.find_mut(pid) {
                        p.stop_reported = true;
                    }
                    Some((pid, found_code, true)) // stopped
                } else {
                    let code = found_code.unwrap_or(0);
                    table.reap(pid);
                    Some((pid, Some(code), false))
                }
            } else {
                None
            }
        };

        if let Some((child_pid, code_opt, stopped)) = result {
            // Restore caller context.
            restore_caller_context(calling_pid, saved_user_rsp);

            // Write wstatus.
            if status_ptr != 0 {
                let wstatus = if stopped {
                    // WIFSTOPPED: (sig << 8) | 0x7f
                    let sig = code_opt.unwrap_or(crate::process::SIGTSTP as i32);
                    (sig & 0xff) << 8 | 0x7f
                } else {
                    let code = code_opt.unwrap_or(0);
                    if code >= 0 {
                        (code & 0xff) << 8 // WIFEXITED
                    } else {
                        (-code) & 0x7f // WIFSIGNALED
                    }
                };
                let bytes = wstatus.to_ne_bytes();
                let _ = crate::mm::user_mem::copy_to_user(status_ptr, &bytes);
            }
            log::info!(
                "[waitpid] pid {} {}",
                child_pid,
                if stopped { "stopped" } else { "exited" }
            );
            return child_pid as u64;
        }

        // No matching child ready.
        if options & WNOHANG != 0 {
            return 0;
        }
        // Yield and try again.
        crate::task::yield_now();
        restore_caller_context(calling_pid, saved_user_rsp);
    }
}

/// Restore the caller's CR3, kernel stack, and user RSP after a yield.
///
/// When a syscall handler calls `yield_now()` to block, another task may
/// enter the kernel via syscall and overwrite the per-core `syscall_user_rsp`
/// and `syscall_stack_top`. This function restores all per-process state
/// so that the `sysretq` return path uses the correct values.
fn restore_caller_context(calling_pid: crate::process::Pid, saved_user_rsp: u64) {
    let (caller_cr3_phys, kstack_top, fs_base) = {
        let table = crate::process::PROCESS_TABLE.lock();
        let cr3 = table.find(calling_pid).and_then(|p| p.page_table_root);
        let kst = table
            .find(calling_pid)
            .map(|p| p.kernel_stack_top)
            .unwrap_or(0);
        let fsb = table.find(calling_pid).map(|p| p.fs_base).unwrap_or(0);
        (cr3, kst, fsb)
    };
    if let Some(phys) = caller_cr3_phys {
        unsafe {
            use x86_64::{
                registers::control::{Cr3, Cr3Flags},
                structures::paging::PhysFrame,
            };
            let frame = PhysFrame::from_start_address(phys).expect("caller cr3 unaligned");
            Cr3::write(frame, Cr3Flags::empty());
        }
    }
    crate::process::set_current_pid(calling_pid);
    // Restore per-core syscall state.
    let data =
        crate::smp::per_core() as *const crate::smp::PerCoreData as *mut crate::smp::PerCoreData;
    unsafe {
        (*data).syscall_user_rsp = saved_user_rsp;
        if kstack_top != 0 {
            (*data).syscall_stack_top = kstack_top;
            crate::smp::set_current_core_kernel_stack(kstack_top);
        }
        // Restore FS.base (TLS pointer) for this process.
        x86_64::registers::model_specific::FsBase::write(x86_64::VirtAddr::new(fs_base));
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check if the current process has pending signals that would interrupt.
///
/// Only returns true for signals whose disposition is not Ignore (e.g.,
/// SIGCHLD defaults to Ignore and should not cause EINTR).
fn has_pending_signal() -> bool {
    let pid = crate::process::current_pid();
    if pid == 0 {
        return false;
    }
    let table = crate::process::PROCESS_TABLE.lock();
    let proc = match table.find(pid) {
        Some(p) => p,
        None => return false,
    };
    if proc.pending_signals == 0 {
        return false;
    }
    // Check if any pending, unblocked signal has a non-Ignore disposition.
    let deliverable = proc.pending_signals & !proc.blocked_signals;
    if deliverable == 0 {
        return false;
    }
    for sig in 0..64u32 {
        if deliverable & (1u64 << sig) != 0 {
            let action = proc.sigaction_get(sig as usize);
            let disposition = match action {
                crate::process::SignalAction::Ignore => {
                    if sig == crate::process::SIGKILL || sig == crate::process::SIGSTOP {
                        return true; // cannot be ignored
                    }
                    crate::process::SignalDisposition::Ignore
                }
                crate::process::SignalAction::Default => crate::process::default_signal_action(sig),
                crate::process::SignalAction::Handler { .. } => return true,
            };
            if disposition != crate::process::SignalDisposition::Ignore {
                return true;
            }
        }
    }
    false
}

/// Copy-on-write clone of user-accessible pages from the parent's page table
/// into the child's page table.
///
/// Instead of copying page contents, both parent and child share the same
/// physical frames.  Writable pages have their WRITABLE bit cleared in both
/// parent and child so that a write triggers a page fault which is resolved
/// by `resolve_cow_fault` in the page fault handler.  Frame reference counts
/// are incremented for each shared frame.
///
/// # Safety
/// The current CR3 must be the parent's page table and `dst_mapper` must
/// reference the child's freshly-allocated PML4.
unsafe fn cow_clone_user_pages(
    phys_off: u64,
    dst_mapper: &mut x86_64::structures::paging::OffsetPageTable<'_>,
) -> Result<(), crate::mm::elf::ElfError> {
    unsafe {
        use x86_64::{
            VirtAddr,
            registers::control::Cr3,
            structures::paging::{Mapper, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB},
        };

        let phys_offset = VirtAddr::new(phys_off);

        let (src_frame, _) = Cr3::read();
        let src_pml4: &PageTable =
            &*(phys_offset + src_frame.start_address().as_u64()).as_ptr::<PageTable>();

        let mut frame_alloc = crate::mm::paging::GlobalFrameAlloc;

        // Walk indices 0–255 (user half).
        for p4 in 0usize..256 {
            let p4e = &src_pml4[p4];
            if !p4e.flags().contains(PageTableFlags::PRESENT) {
                continue;
            }

            let pdpt: &PageTable = &*(phys_offset + p4e.addr().as_u64()).as_ptr::<PageTable>();
            for p3 in 0usize..512 {
                let p3e = &pdpt[p3];
                if !p3e.flags().contains(PageTableFlags::PRESENT) {
                    continue;
                }
                if p3e.flags().contains(PageTableFlags::HUGE_PAGE) {
                    continue;
                }

                let pd: &PageTable = &*(phys_offset + p3e.addr().as_u64()).as_ptr::<PageTable>();
                for p2 in 0usize..512 {
                    let p2e = &pd[p2];
                    if !p2e.flags().contains(PageTableFlags::PRESENT) {
                        continue;
                    }
                    if p2e.flags().contains(PageTableFlags::HUGE_PAGE) {
                        continue;
                    }

                    // Get a mutable reference to the parent's PT so we can clear
                    // WRITABLE on CoW pages.
                    let pt: &mut PageTable =
                        &mut *(phys_offset + p2e.addr().as_u64()).as_mut_ptr::<PageTable>();
                    for p1 in 0usize..512 {
                        let pte = &mut pt[p1];
                        if !pte.flags().contains(PageTableFlags::PRESENT) {
                            continue;
                        }
                        if !pte.flags().contains(PageTableFlags::USER_ACCESSIBLE) {
                            continue;
                        }

                        let vaddr: u64 = ((p4 as u64) << 39)
                            | ((p3 as u64) << 30)
                            | ((p2 as u64) << 21)
                            | ((p1 as u64) << 12);

                        let src_phys = pte.addr();
                        let flags = pte.flags();
                        let was_writable = flags.contains(PageTableFlags::WRITABLE);

                        // Compute child flags: if the page was writable, clear
                        // WRITABLE and set BIT_9 (CoW marker) in the child.
                        // Don't mutate parent PTE yet — defer until map_to succeeds.
                        let child_flags = if was_writable {
                            (flags & !PageTableFlags::WRITABLE) | PageTableFlags::BIT_9
                        } else {
                            flags
                        };

                        // Map the same physical frame in the child.
                        let page = Page::<Size4KiB>::from_start_address(VirtAddr::new(vaddr))
                            .map_err(|_| {
                                crate::mm::elf::ElfError::MappingFailed("invalid vaddr in fork")
                            })?;
                        let frame = PhysFrame::from_start_address(src_phys)
                            .expect("CoW: unaligned frame address");
                        // Intermediate page table entries (PD, PDPT, PML4) must always
                        // have WRITABLE set so that after CoW resolution makes the PTE
                        // writable, writes can actually succeed. The leaf PTE is the
                        // only level that controls CoW (no WRITABLE + BIT_9).
                        let parent_flags = PageTableFlags::PRESENT
                            | PageTableFlags::WRITABLE
                            | PageTableFlags::USER_ACCESSIBLE;
                        dst_mapper
                            .map_to_with_table_flags(
                                page,
                                frame,
                                child_flags,
                                parent_flags,
                                &mut frame_alloc,
                            )
                            .map_err(|_| {
                                crate::mm::elf::ElfError::MappingFailed("map_to failed in cow fork")
                            })?
                            .ignore();

                        // Child mapping succeeded — now mutate the parent PTE to
                        // match (clear WRITABLE, set BIT_9) and bump refcount.
                        if was_writable {
                            pte.set_addr(src_phys, child_flags);
                        }
                        crate::mm::frame_allocator::refcount_inc(src_phys.as_u64());
                    }
                }
            }
        }

        // Flush parent's TLB to ensure CPU sees the cleared WRITABLE bits.
        // A full CR3 reload is the simplest approach.
        let (current_cr3, cr3_flags) = Cr3::read();
        Cr3::write(current_cr3, cr3_flags);

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Initialisation
// ---------------------------------------------------------------------------

pub fn init() {
    let stack_top = gdt::syscall_stack_top();
    // Per-core syscall_stack_top is already set in init_bsp_per_core().
    // Set the legacy TSS RSP0 for interrupt stacks.
    unsafe {
        gdt::set_kernel_stack(stack_top);
    }

    Star::write(
        gdt::user_code_selector(),
        gdt::user_data_selector(),
        gdt::kernel_code_selector(),
        gdt::kernel_data_selector(),
    )
    .expect("STAR MSR write failed: segment selector layout mismatch");

    unsafe extern "C" {
        fn syscall_entry();
    }
    LStar::write(VirtAddr::new(syscall_entry as *const () as u64));
    SFMask::write(RFlags::INTERRUPT_FLAG | RFlags::TRAP_FLAG);
    unsafe {
        Efer::update(|flags| *flags |= EferFlags::SYSTEM_CALL_EXTENSIONS);
    }
}

/// Initialize SYSCALL MSRs on an AP core.
///
/// Sets STAR, LSTAR, SFMASK, and EFER.SCE so that userspace processes
/// dispatched on this core can use the SYSCALL instruction.
/// TSS.RSP0 and per-core syscall_stack_top are handled separately via
/// `set_current_core_kernel_stack` and `set_per_core_syscall_stack_top`.
pub fn init_ap() {
    Star::write(
        gdt::user_code_selector(),
        gdt::user_data_selector(),
        gdt::kernel_code_selector(),
        gdt::kernel_data_selector(),
    )
    .expect("STAR MSR write failed on AP");

    unsafe extern "C" {
        fn syscall_entry();
    }
    LStar::write(VirtAddr::new(syscall_entry as *const () as u64));
    SFMask::write(RFlags::INTERRUPT_FLAG | RFlags::TRAP_FLAG);
    unsafe {
        Efer::update(|flags| *flags |= EferFlags::SYSTEM_CALL_EXTENSIONS);
    }
}

// ===========================================================================
// Phase 12 — Linux-compatible syscall implementations (T013–T026)
// ===========================================================================

// ---------------------------------------------------------------------------
// File descriptor table (P12-T013, T015)
// ---------------------------------------------------------------------------

/// Initial virtual address for the program break (heap).
///
/// Placed at 8 GiB — above typical ELF segments (which load at ~4 MiB) and
/// well below the user stack (at ~128 TiB).
const BRK_BASE: u64 = 0x0000_0002_0000_0000;

/// Initial virtual address for anonymous mmap allocations.
///
/// Placed at 128 GiB — above the brk heap region and below the stack.
const ANON_MMAP_BASE: u64 = 0x0000_0020_0000_0000;

// Re-export FD types from process module (Phase 14 — per-process FD table).
use crate::process::{FdBackend, FdEntry, MAX_FDS};

/// Clone the FD entry at `fd` from the current process's FD table.
///
/// Returns `None` if no process is running or the FD slot is empty.
/// Uses the shared fd table when the process is part of a thread group
/// created with `CLONE_FILES`.
fn current_fd_entry(fd: usize) -> Option<FdEntry> {
    let pid = crate::process::current_pid();
    let table = crate::process::PROCESS_TABLE.lock();
    let proc = table.find(pid)?;
    proc.fd_get(fd)
}

/// Mutate the FD entry at `fd` in the current process's FD table.
/// Uses the shared fd table when the process is part of a thread group
/// created with `CLONE_FILES`.
fn with_current_fd_mut<F: FnOnce(&mut Option<FdEntry>)>(fd: usize, f: F) {
    let pid = crate::process::current_pid();
    let mut table = crate::process::PROCESS_TABLE.lock();
    if let Some(proc) = table.find_mut(pid) {
        if let Some(shared) = &proc.shared_fd_table {
            if fd < MAX_FDS {
                f(&mut shared.lock()[fd]);
            }
        } else if let Some(slot) = proc.fd_table.get_mut(fd) {
            f(slot);
        }
    }
}

/// Allocate the lowest available FD slot (starting from `min_fd`) in the
/// current process's FD table. Returns the FD number or `None` if full.
/// Uses the shared fd table when the process is part of a thread group
/// created with `CLONE_FILES`.
fn alloc_fd(min_fd: usize, entry: FdEntry) -> Option<usize> {
    let pid = crate::process::current_pid();
    let mut table = crate::process::PROCESS_TABLE.lock();
    let proc = table.find_mut(pid)?;
    proc.fd_alloc(min_fd, entry)
}

fn seed_pseudorandom_state() -> u64 {
    let mut state: u64 = unsafe { core::arch::x86_64::_rdtsc() };
    if state == 0 {
        state = 0xDEAD_BEEF_CAFE_BABE;
    }
    state
}

fn fill_pseudorandom_bytes(state: &mut u64, out: &mut [u8]) {
    for byte in out.iter_mut() {
        *state ^= *state >> 12;
        *state ^= *state << 25;
        *state ^= *state >> 27;
        *byte = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
    }
}

fn copy_byte_pattern_to_user(buf_ptr: u64, count: usize, byte: u8) -> Result<(), ()> {
    let chunk = [byte; 256];
    let mut written = 0usize;
    while written < count {
        let len = (count - written).min(chunk.len());
        if crate::mm::user_mem::copy_to_user(buf_ptr + written as u64, &chunk[..len]).is_err() {
            return Err(());
        }
        written += len;
    }
    Ok(())
}

fn copy_pseudorandom_to_user(buf_ptr: u64, count: usize) -> Result<(), ()> {
    let mut state = seed_pseudorandom_state();
    let mut chunk = [0u8; 256];
    let mut written = 0usize;
    while written < count {
        let len = (count - written).min(chunk.len());
        fill_pseudorandom_bytes(&mut state, &mut chunk[..len]);
        if crate::mm::user_mem::copy_to_user(buf_ptr + written as u64, &chunk[..len]).is_err() {
            return Err(());
        }
        written += len;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// T013: read(fd, buf, count)
// ---------------------------------------------------------------------------

fn sys_linux_read(fd: u64, buf_ptr: u64, count: u64) -> u64 {
    let fd = fd as usize;
    if fd >= MAX_FDS {
        return NEG_EBADF;
    }

    let entry = match current_fd_entry(fd) {
        Some(e) => e,
        None => return NEG_EBADF,
    };

    if !entry.readable {
        return NEG_EBADF;
    }

    match &entry.backend {
        FdBackend::Stdin | FdBackend::DeviceTTY { .. } => {
            // Read from kernel stdin buffer.
            let capped = (count as usize).min(4096);
            let nonblock = entry.nonblock;
            let pid = crate::process::current_pid();
            let saved_user_rsp = per_core_syscall_user_rsp();
            loop {
                if crate::stdin::has_data() {
                    let mut tmp = [0u8; 4096];
                    let n = crate::stdin::read(&mut tmp[..capped]);
                    if n > 0 {
                        if crate::mm::user_mem::copy_to_user(buf_ptr, &tmp[..n]).is_err() {
                            return NEG_EFAULT;
                        }
                        return n as u64;
                    }
                }
                if nonblock {
                    return NEG_EAGAIN;
                }
                // Check for pending signals so Ctrl-C works while blocked.
                if has_pending_signal() {
                    return NEG_EINTR;
                }
                crate::task::yield_now();
                restore_caller_context(pid, saved_user_rsp);
            }
        }
        FdBackend::Stdout => NEG_EBADF,
        FdBackend::Ramdisk {
            content_addr,
            content_len,
        } => {
            let remaining = content_len.saturating_sub(entry.offset);
            let to_read = (count as usize).min(remaining).min(64 * 1024);
            if to_read == 0 {
                return 0; // EOF
            }

            // SAFETY: content_addr is a static ramdisk pointer (lives forever).
            let src = unsafe {
                core::slice::from_raw_parts((*content_addr + entry.offset) as *const u8, to_read)
            };

            if crate::mm::user_mem::copy_to_user(buf_ptr, src).is_err() {
                return NEG_EFAULT;
            }

            with_current_fd_mut(fd, |slot| {
                if let Some(e) = slot {
                    e.offset += to_read;
                }
            });
            to_read as u64
        }
        FdBackend::Tmpfs { path } => {
            // Cap count at 64 KiB to match ramdisk path and prevent overflow.
            let capped_count = (count as usize).min(64 * 1024);
            let tmpfs = crate::fs::tmpfs::TMPFS.lock();
            let data = match tmpfs.read_file(path, entry.offset, capped_count) {
                Ok(d) => d,
                Err(crate::fs::tmpfs::TmpfsError::NotFound) => return NEG_ENOENT,
                Err(_) => return NEG_EBADF,
            };
            let to_read = data.len();
            if to_read == 0 {
                return 0; // EOF
            }

            if crate::mm::user_mem::copy_to_user(buf_ptr, data).is_err() {
                return NEG_EFAULT;
            }

            drop(tmpfs);
            with_current_fd_mut(fd, |slot| {
                if let Some(e) = slot {
                    e.offset += to_read;
                }
            });
            to_read as u64
        }
        FdBackend::Proc { path, snapshot } => {
            let generated;
            let data: &[u8] = match snapshot.as_deref() {
                Some(data) => data,
                None => {
                    generated = match crate::fs::procfs::read_file(path) {
                        Some(data) => data,
                        None => return NEG_ENOENT,
                    };
                    &generated
                }
            };
            let offset = entry.offset.min(data.len());
            let to_read = (count as usize).min(data.len().saturating_sub(offset));
            if to_read == 0 {
                return 0;
            }

            if crate::mm::user_mem::copy_to_user(buf_ptr, &data[offset..offset + to_read]).is_err()
            {
                return NEG_EFAULT;
            }

            with_current_fd_mut(fd, |slot| {
                if let Some(e) = slot {
                    e.offset += to_read;
                }
            });
            to_read as u64
        }
        FdBackend::Fat32Disk {
            start_cluster,
            file_size,
            ..
        } => {
            let capped_count = (count as usize).min(64 * 1024);
            let start_cluster = *start_cluster;
            let file_size = *file_size;
            let offset = entry.offset;

            if start_cluster < 2 || offset >= file_size as usize {
                return 0; // EOF or empty file
            }

            let vol = crate::fs::fat32::FAT32_VOLUME.lock();
            if let Some(vol) = vol.as_ref() {
                let mut read_buf = alloc::vec![0u8; capped_count];
                match vol.read_file(start_cluster, file_size, offset, &mut read_buf) {
                    Ok(0) => 0,
                    Ok(n) => {
                        if crate::mm::user_mem::copy_to_user(buf_ptr, &read_buf[..n]).is_err() {
                            return NEG_EFAULT;
                        }

                        with_current_fd_mut(fd, |slot| {
                            if let Some(e) = slot {
                                e.offset += n;
                            }
                        });
                        n as u64
                    }
                    Err(_) => NEG_EIO,
                }
            } else {
                NEG_EIO
            }
        }
        FdBackend::PipeRead { pipe_id } => {
            let pipe_id = *pipe_id;
            let nonblock = entry.nonblock;
            let capped = (count as usize).min(4096);
            let pid = crate::process::current_pid();
            let saved_user_rsp = per_core_syscall_user_rsp();
            // Yield-loop until data is available or writer closes.
            loop {
                let mut tmp = [0u8; 4096];
                match crate::pipe::pipe_read(pipe_id, &mut tmp[..capped]) {
                    Ok(0) => return 0, // EOF
                    Ok(n) => {
                        if crate::mm::user_mem::copy_to_user(buf_ptr, &tmp[..n]).is_err() {
                            return NEG_EFAULT;
                        }
                        return n as u64;
                    }
                    Err(_would_block) => {
                        if nonblock {
                            return NEG_EAGAIN;
                        }
                        if has_pending_signal() {
                            return NEG_EINTR;
                        }
                        crate::task::yield_now();
                        restore_caller_context(pid, saved_user_rsp);
                    }
                }
            }
        }
        FdBackend::Ext2Disk { inode_num, .. } => {
            let capped_count = (count as usize).min(64 * 1024);
            let inode_num = *inode_num;
            let offset = entry.offset;

            let vol = crate::fs::ext2::EXT2_VOLUME.lock();
            if let Some(vol) = vol.as_ref() {
                match vol.read_inode(inode_num) {
                    Ok(inode) => {
                        let actual_size = inode.size as usize;
                        if offset >= actual_size {
                            return 0;
                        }
                        let mut read_buf = alloc::vec![0u8; capped_count];
                        match vol.read_file_data(&inode, offset as u64, &mut read_buf) {
                            Ok(0) => 0,
                            Ok(n) => {
                                if crate::mm::user_mem::copy_to_user(buf_ptr, &read_buf[..n])
                                    .is_err()
                                {
                                    return NEG_EFAULT;
                                }
                                with_current_fd_mut(fd, |slot| {
                                    if let Some(e) = slot {
                                        e.offset += n;
                                        if let FdBackend::Ext2Disk {
                                            file_size: ref mut fs,
                                            ..
                                        } = e.backend
                                        {
                                            *fs = inode.size;
                                        }
                                    }
                                });
                                n as u64
                            }
                            Err(_) => NEG_EIO,
                        }
                    }
                    Err(_) => NEG_EIO,
                }
            } else {
                NEG_EIO
            }
        }
        FdBackend::PipeWrite { .. } => NEG_EBADF,
        FdBackend::Dir { .. } => NEG_EISDIR,
        FdBackend::DevNull => 0, // EOF
        FdBackend::DevZero | FdBackend::DevFull => {
            // /dev/zero and /dev/full behave like infinite zero-filled files.
            if copy_byte_pattern_to_user(buf_ptr, count as usize, 0).is_err() {
                return NEG_EFAULT;
            }
            count
        }
        FdBackend::DevUrandom => {
            if copy_pseudorandom_to_user(buf_ptr, count as usize).is_err() {
                return NEG_EFAULT;
            }
            count
        }
        FdBackend::PtyMaster { pty_id } => {
            if count == 0 {
                return 0;
            }
            // Master reads from s2m (slave-to-master) buffer.
            let pty_id = *pty_id;
            let nonblock = entry.nonblock;
            let pid = crate::process::current_pid();
            let saved_user_rsp = per_core_syscall_user_rsp();
            loop {
                {
                    let mut table = crate::pty::PTY_TABLE.lock();
                    if let Some(Some(pair)) = table.get_mut(pty_id as usize) {
                        if !pair.s2m.is_empty() {
                            let mut dst = [0u8; 4096];
                            let to_read = count.min(dst.len() as u64) as usize;
                            let n = pair.s2m.read(&mut dst[..to_read]);
                            drop(table);
                            // Reading from s2m frees space for slave writers.
                            crate::pty::wake_slave(pty_id);
                            if crate::mm::user_mem::copy_to_user(buf_ptr, &dst[..n]).is_err() {
                                return NEG_EFAULT;
                            }
                            return n as u64;
                        }
                        if pair.slave_refcount == 0 && pair.slave_opened {
                            return 0; // EOF — slave closed
                        }
                    } else {
                        return 0; // PTY freed
                    }
                }
                if nonblock {
                    return NEG_EAGAIN;
                }
                if has_pending_signal() {
                    return NEG_EINTR;
                }
                crate::task::yield_now();
                restore_caller_context(pid, saved_user_rsp);
            }
        }
        FdBackend::PtySlave { pty_id } => {
            if count == 0 {
                return 0;
            }
            // Slave reads from m2s (master-to-slave) buffer via line discipline.
            let pty_id = *pty_id;
            let nonblock = entry.nonblock;
            let pid = crate::process::current_pid();
            let saved_user_rsp = per_core_syscall_user_rsp();
            loop {
                {
                    let mut table = crate::pty::PTY_TABLE.lock();
                    if let Some(Some(pair)) = table.get_mut(pty_id as usize) {
                        if pair.termios.is_canonical() {
                            // Canonical mode: check edit buffer for complete line.
                            let line = pair.edit_buf.as_slice();
                            let has_line = line.contains(&b'\n');
                            if has_line {
                                let eol = line.iter().position(|&b| b == b'\n').unwrap() + 1;
                                let to_copy = eol.min(count as usize).min(4096);
                                let mut dst = [0u8; 4096];
                                dst[..to_copy]
                                    .copy_from_slice(&pair.edit_buf.as_slice()[..to_copy]);
                                pair.edit_buf.drain(to_copy);
                                drop(table);
                                // Draining edit_buf frees space for master writers.
                                crate::pty::wake_master(pty_id);
                                if crate::mm::user_mem::copy_to_user(buf_ptr, &dst[..to_copy])
                                    .is_err()
                                {
                                    return NEG_EFAULT;
                                }
                                return to_copy as u64;
                            }
                            // VEOF (^D) on empty line → return 0 (EOF).
                            if pair.eof_pending {
                                pair.eof_pending = false;
                                drop(table);
                                return 0;
                            }
                        } else {
                            // Raw mode: read directly from m2s.
                            if !pair.m2s.is_empty() {
                                let mut dst = [0u8; 4096];
                                let to_read = count.min(dst.len() as u64) as usize;
                                let n = pair.m2s.read(&mut dst[..to_read]);
                                drop(table);
                                // Reading from m2s frees space for master writers.
                                crate::pty::wake_master(pty_id);
                                if crate::mm::user_mem::copy_to_user(buf_ptr, &dst[..n]).is_err() {
                                    return NEG_EFAULT;
                                }
                                return n as u64;
                            }
                        }
                        if pair.master_refcount == 0 {
                            return 0; // EOF — master closed
                        }
                    } else {
                        return 0; // PTY freed
                    }
                }
                if nonblock {
                    return NEG_EAGAIN;
                }
                if has_pending_signal() {
                    return NEG_EINTR;
                }
                crate::task::yield_now();
                restore_caller_context(pid, saved_user_rsp);
            }
        }
        FdBackend::Socket { .. } => {
            // Delegate to recvfrom with no addr
            sys_recvfrom_socket(fd as u64, buf_ptr, count, 0, 0, 0)
        }
        FdBackend::UnixSocket { handle } => {
            if count == 0 {
                return 0;
            }
            let handle = *handle;
            let nonblock = entry.nonblock;
            let pid = crate::process::current_pid();
            let saved_user_rsp = crate::smp::per_core().syscall_user_rsp;
            let capped = (count as usize).min(4096);
            let sock_type = crate::net::unix::with_unix_socket(handle, |s| s.socket_type);
            match sock_type {
                Some(crate::net::unix::UnixSocketType::Stream) => {
                    let mut tmp = alloc::vec![0u8; capped];
                    loop {
                        match crate::net::unix::unix_stream_read(handle, &mut tmp) {
                            Ok(0) => return 0,
                            Ok(n) => {
                                if crate::mm::user_mem::copy_to_user(buf_ptr, &tmp[..n]).is_err() {
                                    return NEG_EFAULT;
                                }
                                return n as u64;
                            }
                            Err(-11) => {
                                // EAGAIN — no data yet, block or return.
                                if nonblock {
                                    return NEG_EAGAIN;
                                }
                                if has_pending_signal() {
                                    return NEG_EINTR;
                                }
                                crate::net::unix::UNIX_SOCKET_WAITQUEUES[handle].sleep();
                                restore_caller_context(pid, saved_user_rsp);
                            }
                            Err(e) => return e as u64, // ENOTCONN, etc.
                        }
                    }
                }
                Some(crate::net::unix::UnixSocketType::Datagram) => {
                    let mut tmp = alloc::vec![0u8; capped];
                    loop {
                        match crate::net::unix::unix_dgram_recv(handle, &mut tmp) {
                            Ok((n, _sender)) => {
                                if crate::mm::user_mem::copy_to_user(buf_ptr, &tmp[..n]).is_err() {
                                    return NEG_EFAULT;
                                }
                                return n as u64;
                            }
                            Err(-11) => {
                                if nonblock {
                                    return NEG_EAGAIN;
                                }
                                if has_pending_signal() {
                                    return NEG_EINTR;
                                }
                                crate::net::unix::UNIX_SOCKET_WAITQUEUES[handle].sleep();
                                restore_caller_context(pid, saved_user_rsp);
                            }
                            Err(e) => return e as u64,
                        }
                    }
                }
                None => NEG_EBADF,
            }
        }
        FdBackend::Epoll { .. } => NEG_EBADF,
    }
}

// ---------------------------------------------------------------------------
// T014: write(fd, buf, count)
// ---------------------------------------------------------------------------

fn sys_linux_write(fd: u64, buf_ptr: u64, count: u64) -> u64 {
    let fd_idx = fd as usize;
    if fd_idx >= MAX_FDS {
        return NEG_EBADF;
    }

    let entry = match current_fd_entry(fd_idx) {
        Some(e) => e,
        None => return NEG_EBADF,
    };

    if !entry.writable {
        return NEG_EBADF;
    }

    match &entry.backend {
        FdBackend::Stdout | FdBackend::DeviceTTY { .. } => {
            // stdout/stderr/tty go to serial + framebuffer console.
            let len = (count as usize).min(4096);
            let mut buf = [0u8; 4096];
            if crate::mm::user_mem::copy_from_user(&mut buf[..len], buf_ptr).is_err() {
                return NEG_EFAULT;
            }
            if let Ok(s) = core::str::from_utf8(&buf[..len]) {
                crate::serial::_print(format_args!("{}", s));
                crate::fb::write_str(s);
            }
            len as u64
        }
        FdBackend::Stdin => NEG_EBADF,
        FdBackend::Ramdisk { .. } => NEG_EBADF, // ramdisk is read-only
        FdBackend::Proc { .. } => NEG_EBADF,
        FdBackend::Tmpfs { path } => {
            let len = (count as usize).min(64 * 1024);
            let mut buf = [0u8; 4096];
            let mut written = 0usize;
            let mut offset = entry.offset;

            // Write in 4 KiB chunks to avoid huge stack buffers.
            while written < len {
                let chunk = (len - written).min(4096);
                let user_ptr = match buf_ptr.checked_add(written as u64) {
                    Some(p) => p,
                    None => {
                        if written == 0 {
                            return NEG_EFAULT;
                        }
                        break;
                    }
                };
                if crate::mm::user_mem::copy_from_user(&mut buf[..chunk], user_ptr).is_err() {
                    if written == 0 {
                        return NEG_EFAULT;
                    }
                    break;
                }
                let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
                if let Err(e) = tmpfs.write_file(path, offset, &buf[..chunk]) {
                    if written == 0 {
                        return match e {
                            crate::fs::tmpfs::TmpfsError::NoSpace => NEG_ENOSPC,
                            crate::fs::tmpfs::TmpfsError::NotFound => NEG_EBADF,
                            _ => NEG_EINVAL,
                        };
                    }
                    break;
                }
                drop(tmpfs);
                written += chunk;
                offset += chunk;
            }

            with_current_fd_mut(fd_idx, |slot| {
                if let Some(e) = slot {
                    e.offset = offset;
                }
            });
            written as u64
        }
        FdBackend::Fat32Disk {
            path,
            start_cluster,
            file_size,
            dir_cluster,
        } => {
            let len = (count as usize).min(64 * 1024);
            let path = path.clone();
            let start_cluster = *start_cluster;
            let current_file_size = *file_size as usize;
            let dir_cluster = *dir_cluster;
            let offset = entry.offset;

            // Read user data in 4 KiB chunks.
            let mut data = alloc::vec![0u8; len];
            let mut copied = 0usize;
            while copied < len {
                let chunk = (len - copied).min(4096);
                let user_ptr = match buf_ptr.checked_add(copied as u64) {
                    Some(p) => p,
                    None => {
                        if copied == 0 {
                            return NEG_EFAULT;
                        }
                        break;
                    }
                };
                let mut tmp = [0u8; 4096];
                if crate::mm::user_mem::copy_from_user(&mut tmp[..chunk], user_ptr).is_err() {
                    if copied == 0 {
                        return NEG_EFAULT;
                    }
                    break;
                }
                data[copied..copied + chunk].copy_from_slice(&tmp[..chunk]);
                copied += chunk;
            }
            let data = &data[..copied];

            let mut vol = crate::fs::fat32::FAT32_VOLUME.lock();
            if let Some(vol) = vol.as_mut() {
                match vol.write_file(start_cluster, offset, data, current_file_size) {
                    Ok((new_start, new_size)) => {
                        // Extract filename from path for dir entry update.
                        let file_name = path.rsplit('/').next().unwrap_or(&path);
                        if vol
                            .update_dir_entry(dir_cluster, file_name, new_start, new_size as u32)
                            .is_err()
                        {
                            return NEG_EIO;
                        }

                        let new_offset = offset + copied;
                        with_current_fd_mut(fd_idx, |slot| {
                            if let Some(e) = slot {
                                e.offset = new_offset;
                                if let FdBackend::Fat32Disk {
                                    start_cluster: ref mut sc,
                                    file_size: ref mut fs,
                                    ..
                                } = e.backend
                                {
                                    *sc = new_start;
                                    *fs = new_size as u32;
                                }
                            }
                        });
                        copied as u64
                    }
                    Err(_) => NEG_EIO,
                }
            } else {
                NEG_EIO
            }
        }
        FdBackend::Ext2Disk {
            inode_num,
            file_size,
            ..
        } => {
            let len = (count as usize).min(64 * 1024);
            let inode_num = *inode_num;
            let _current_file_size = *file_size as usize;
            let offset = entry.offset;

            let mut data = alloc::vec![0u8; len];
            let mut copied = 0usize;
            while copied < len {
                let chunk = (len - copied).min(4096);
                let user_ptr = match buf_ptr.checked_add(copied as u64) {
                    Some(p) => p,
                    None => {
                        if copied == 0 {
                            return NEG_EFAULT;
                        }
                        break;
                    }
                };
                let mut tmp = [0u8; 4096];
                if crate::mm::user_mem::copy_from_user(&mut tmp[..chunk], user_ptr).is_err() {
                    if copied == 0 {
                        return NEG_EFAULT;
                    }
                    break;
                }
                data[copied..copied + chunk].copy_from_slice(&tmp[..chunk]);
                copied += chunk;
            }
            let data = &data[..copied];

            let mut vol = crate::fs::ext2::EXT2_VOLUME.lock();
            if let Some(vol) = vol.as_mut() {
                match vol.read_inode(inode_num) {
                    Ok(mut inode) => {
                        // Phase 32: update mtime/ctime on write
                        let now = current_unix_time();
                        inode.mtime = now;
                        inode.ctime = now;
                        match vol.write_file_data(inode_num, &mut inode, offset as u64, data) {
                            Ok(n) => {
                                let new_offset = offset + n;
                                let new_size = inode.size;
                                with_current_fd_mut(fd_idx, |slot| {
                                    if let Some(e) = slot {
                                        e.offset = new_offset;
                                        if let FdBackend::Ext2Disk {
                                            file_size: ref mut fs,
                                            ..
                                        } = e.backend
                                        {
                                            *fs = new_size;
                                        }
                                    }
                                });
                                n as u64
                            }
                            Err(_) => NEG_EIO,
                        }
                    }
                    Err(_) => NEG_EIO,
                }
            } else {
                NEG_EIO
            }
        }
        FdBackend::PipeWrite { pipe_id } => {
            let pipe_id = *pipe_id;
            let nonblock = entry.nonblock;
            let len = (count as usize).min(4096);
            let mut buf = [0u8; 4096];
            if crate::mm::user_mem::copy_from_user(&mut buf[..len], buf_ptr).is_err() {
                return NEG_EFAULT;
            }
            let pid = crate::process::current_pid();
            let saved_user_rsp = per_core_syscall_user_rsp();
            // Yield-loop until space is available or reader closes.
            loop {
                match crate::pipe::pipe_write(pipe_id, &buf[..len]) {
                    Ok(n) => return n as u64,
                    Err(false) => {
                        // Reader closed — EPIPE.
                        const NEG_EPIPE: u64 = (-32_i64) as u64;
                        return NEG_EPIPE;
                    }
                    Err(true) => {
                        if nonblock {
                            return NEG_EAGAIN;
                        }
                        // Would block — yield and retry.
                        if has_pending_signal() {
                            return NEG_EINTR;
                        }
                        crate::task::yield_now();
                        restore_caller_context(pid, saved_user_rsp);
                    }
                }
            }
        }
        FdBackend::PipeRead { .. } => NEG_EBADF,
        FdBackend::Dir { .. } => NEG_EBADF,
        FdBackend::DevNull | FdBackend::DevZero | FdBackend::DevUrandom => count, // silently discard
        FdBackend::DevFull => NEG_ENOSPC, // no space left on device
        FdBackend::PtyMaster { pty_id } => {
            // Master writes to m2s (master-to-slave) buffer.
            // Apply line discipline on the slave side (input processing).
            let pty_id = *pty_id;
            let mut src_data = alloc::vec![0u8; count.min(4096) as usize];
            if crate::mm::user_mem::copy_from_user(&mut src_data, buf_ptr).is_err() {
                return NEG_EFAULT;
            }
            let mut table = crate::pty::PTY_TABLE.lock();
            if let Some(Some(pair)) = table.get_mut(pty_id as usize) {
                if pair.slave_refcount == 0 && !pair.locked {
                    drop(table);
                    return NEG_EIO;
                }
                let is_canonical = pair.termios.is_canonical();
                let is_echo = pair.termios.is_echo();
                let is_isig = pair.termios.is_isig();
                let echoe = pair.termios.c_lflag & kernel_core::tty::ECHOE != 0;
                let echok = pair.termios.c_lflag & kernel_core::tty::ECHOK != 0;
                let echonl = pair.termios.c_lflag & kernel_core::tty::ECHONL != 0;
                let icrnl = pair.termios.c_iflag & kernel_core::tty::ICRNL != 0;
                let inlcr = pair.termios.c_iflag & kernel_core::tty::INLCR != 0;
                let igncr = pair.termios.c_iflag & kernel_core::tty::IGNCR != 0;
                let vintr = pair.termios.c_cc[kernel_core::tty::VINTR];
                let vquit = pair.termios.c_cc[kernel_core::tty::VQUIT];
                let vsusp = pair.termios.c_cc[kernel_core::tty::VSUSP];
                let verase = pair.termios.c_cc[kernel_core::tty::VERASE];
                let vkill = pair.termios.c_cc[kernel_core::tty::VKILL];
                let vwerase = pair.termios.c_cc[kernel_core::tty::VWERASE];
                let veof = pair.termios.c_cc[kernel_core::tty::VEOF];
                let fg_pgid = pair.slave_fg_pgid;

                let mut written = 0usize;
                for &byte in &src_data {
                    // Input flag transformations.
                    let mut b = byte;
                    if b == b'\r' {
                        if igncr {
                            written += 1;
                            continue;
                        }
                        if icrnl {
                            b = b'\n';
                        }
                    } else if b == b'\n' && inlcr {
                        b = b'\r';
                    }

                    // Signal generation (ISIG).
                    if is_isig {
                        if b == vintr {
                            if fg_pgid != 0 {
                                drop(table);
                                crate::process::send_signal_to_group(
                                    fg_pgid,
                                    crate::process::SIGINT,
                                );
                                table = crate::pty::PTY_TABLE.lock();
                            }
                            written += 1;
                            continue;
                        }
                        if b == vquit {
                            if fg_pgid != 0 {
                                drop(table);
                                crate::process::send_signal_to_group(
                                    fg_pgid,
                                    crate::process::SIGQUIT,
                                );
                                table = crate::pty::PTY_TABLE.lock();
                            }
                            written += 1;
                            continue;
                        }
                        if b == vsusp {
                            if fg_pgid != 0 {
                                drop(table);
                                crate::process::send_signal_to_group(
                                    fg_pgid,
                                    crate::process::SIGTSTP,
                                );
                                table = crate::pty::PTY_TABLE.lock();
                            }
                            written += 1;
                            continue;
                        }
                    }

                    // Re-acquire pair reference after potential drop/reacquire.
                    let pair = match table.get_mut(pty_id as usize).and_then(|s| s.as_mut()) {
                        Some(p) => p,
                        None => return written as u64,
                    };

                    if is_canonical {
                        // Canonical mode: buffer in edit_buf.
                        if b == verase {
                            if pair.edit_buf.erase_char().is_some() && is_echo && echoe {
                                pair.s2m.write(b"\x08 \x08");
                            }
                        } else if b == vkill {
                            let n = pair.edit_buf.kill_line();
                            if is_echo {
                                if echok {
                                    pair.s2m.write(b"\n");
                                } else {
                                    for _ in 0..n {
                                        pair.s2m.write(b"\x08 \x08");
                                    }
                                }
                            }
                        } else if b == vwerase {
                            let n = pair.edit_buf.word_erase();
                            if is_echo {
                                for _ in 0..n {
                                    pair.s2m.write(b"\x08 \x08");
                                }
                            }
                        } else if b == veof {
                            // ^D: if edit buffer has content, flush as a line.
                            // If empty, signal EOF to the reader.
                            if !pair.edit_buf.is_empty() {
                                if !pair.edit_buf.push(b'\n') {
                                    // Edit buffer full — stop without counting this byte.
                                    break;
                                }
                            } else {
                                pair.eof_pending = true;
                            }
                            // Don't echo ^D.
                        } else {
                            if !pair.edit_buf.push(b) {
                                // Edit buffer full — stop without counting this byte.
                                break;
                            }
                            if is_echo {
                                if b == b'\n' || echonl || b >= 0x20 {
                                    pair.s2m.write(&[b]);
                                } else {
                                    // Echo control chars as ^X.
                                    pair.s2m.write(&[b'^', b + 0x40]);
                                }
                            }
                        }
                    } else {
                        // Raw mode: write directly to m2s.
                        if pair.m2s.write(&[b]) == 0 {
                            break; // buffer full
                        }
                        if is_echo {
                            pair.s2m.write(&[b]);
                        }
                    }
                    written += 1;
                }
                drop(table);
                // Wake slave waiters (data written to m2s / edit_buf).
                crate::pty::wake_slave(pty_id);
                // Wake master waiters (echo may have written to s2m).
                crate::pty::wake_master(pty_id);
                written as u64
            } else {
                NEG_EIO
            }
        }
        FdBackend::PtySlave { pty_id } => {
            // Slave writes to s2m (slave-to-master) buffer.
            // Apply output processing (OPOST).
            let pty_id = *pty_id;
            let mut src_data = alloc::vec![0u8; count.min(4096) as usize];
            if crate::mm::user_mem::copy_from_user(&mut src_data, buf_ptr).is_err() {
                return NEG_EFAULT;
            }
            let mut table = crate::pty::PTY_TABLE.lock();
            if let Some(Some(pair)) = table.get_mut(pty_id as usize) {
                if pair.master_refcount == 0 {
                    return NEG_EIO;
                }
                let opost = pair.termios.c_oflag & kernel_core::tty::OPOST != 0;
                let onlcr = pair.termios.c_oflag & kernel_core::tty::ONLCR != 0;
                let mut written = 0usize;
                for &b in &src_data {
                    if opost && onlcr && b == b'\n' {
                        // Ensure atomic CR+LF: need at least 2 bytes of space.
                        if pair.s2m.space() < 2 {
                            break;
                        }
                        pair.s2m.write(b"\r");
                        pair.s2m.write(b"\n");
                    } else if pair.s2m.write(&[b]) == 0 {
                        break;
                    }
                    written += 1;
                }
                drop(table);
                // Wake master waiters (data written to s2m).
                crate::pty::wake_master(pty_id);
                written as u64
            } else {
                NEG_EIO
            }
        }
        FdBackend::Socket { .. } => {
            // Delegate to sendto with no addr
            sys_sendto(fd, buf_ptr, count, 0, 0, 0)
        }
        FdBackend::UnixSocket { handle } => {
            let handle = *handle;
            let nonblock = entry.nonblock;
            let pid = crate::process::current_pid();
            let saved_user_rsp = crate::smp::per_core().syscall_user_rsp;
            let capped = (count as usize).min(4096);
            let mut data = alloc::vec![0u8; capped];
            if crate::mm::user_mem::copy_from_user(&mut data, buf_ptr).is_err() {
                return NEG_EFAULT;
            }
            let sock_type = crate::net::unix::with_unix_socket(handle, |s| s.socket_type);
            match sock_type {
                Some(crate::net::unix::UnixSocketType::Stream) => loop {
                    match crate::net::unix::unix_stream_write(handle, &data) {
                        Ok(n) => return n as u64,
                        Err(-11) => {
                            // EAGAIN — buffer full, block or return.
                            if nonblock {
                                return NEG_EAGAIN;
                            }
                            if has_pending_signal() {
                                return NEG_EINTR;
                            }
                            crate::net::unix::UNIX_SOCKET_WAITQUEUES[handle].sleep();
                            restore_caller_context(pid, saved_user_rsp);
                        }
                        Err(e) => return e as u64, // EPIPE, ENOTCONN, etc.
                    }
                },
                Some(crate::net::unix::UnixSocketType::Datagram) => {
                    // For connected datagram sockets, send to peer
                    let peer = crate::net::unix::with_unix_socket(handle, |s| s.peer).flatten();
                    let sender_path =
                        crate::net::unix::with_unix_socket(handle, |s| s.path.clone()).flatten();
                    match peer {
                        Some(target) => loop {
                            match crate::net::unix::unix_dgram_send(
                                sender_path.clone(),
                                target,
                                &data,
                            ) {
                                Ok(n) => {
                                    crate::net::unix::wake_unix_socket(target);
                                    return n as u64;
                                }
                                Err(-11) => {
                                    if nonblock {
                                        return NEG_EAGAIN;
                                    }
                                    if has_pending_signal() {
                                        return NEG_EINTR;
                                    }
                                    crate::net::unix::UNIX_SOCKET_WAITQUEUES[target].sleep();
                                    restore_caller_context(pid, saved_user_rsp);
                                }
                                Err(e) => return e as u64,
                            }
                        },
                        None => NEG_ENOTCONN,
                    }
                }
                None => NEG_EBADF,
            }
        }
        FdBackend::Epoll { .. } => NEG_EBADF,
    }
}

// ---------------------------------------------------------------------------
// T015: open(path, flags) / openat delegates here
// ---------------------------------------------------------------------------

/// Read a null-terminated C string from userspace into `buf`.
///
/// Copies one byte at a time to handle page boundaries gracefully.
/// Returns the UTF-8 string on success, or `None` if the pointer is invalid,
/// the string exceeds `buf.len()`, or the bytes are not valid UTF-8.
fn read_user_cstr<const N: usize>(ptr: u64, buf: &mut [u8; N]) -> Option<&str> {
    if ptr == 0 {
        return None;
    }
    let mut len = 0usize;
    while len < buf.len() {
        let mut b = [0u8; 1];
        let addr = ptr.checked_add(len as u64)?;
        if crate::mm::user_mem::copy_from_user(&mut b, addr).is_err() {
            return None;
        }
        if b[0] == 0 {
            break;
        }
        buf[len] = b[0];
        len += 1;
    }
    if len >= buf.len() {
        return None; // no NUL terminator found within buffer
    }
    if len == 0 {
        return Some("");
    }
    core::str::from_utf8(&buf[..len]).ok()
}

/// Linux open flags.
const O_CREAT: u64 = 0o100;
const O_TRUNC: u64 = 0o1000;
const O_APPEND: u64 = 0o2000;
const O_DIRECTORY: u64 = 0o200000;
#[allow(dead_code)]
const O_NOFOLLOW: u64 = 0o400000;

/// `AT_FDCWD` sentinel: resolve relative paths against the process's cwd.
const AT_FDCWD: u64 = (-100_i64) as u64;
const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
const AT_SYMLINK_FOLLOW: u64 = 0x400;

/// Check if a resolved absolute path is a directory across all filesystems.
fn is_directory(path: &str) -> bool {
    if path == "/" {
        return true;
    }
    if crate::fs::procfs::is_dir(path) {
        return true;
    }
    if let Some(rel) = tmpfs_relative_path(path) {
        if rel.is_empty() {
            return true; // /tmp itself
        }
        let tmpfs = crate::fs::tmpfs::TMPFS.lock();
        return tmpfs.stat(rel).map(|s| s.is_dir).unwrap_or(false);
    }
    // Check ramdisk first (overlays /bin, /sbin).
    if let Some(node) = crate::fs::ramdisk::ramdisk_lookup(path) {
        return node.is_dir();
    }
    // ext2 root filesystem.
    if crate::fs::ext2::is_mounted()
        && let Some(rel) = ext2_root_path(path)
    {
        let vol = crate::fs::ext2::EXT2_VOLUME.lock();
        if let Some(vol) = vol.as_ref() {
            return vol.is_dir(rel);
        }
    }
    // Legacy: /data paths for FAT32 fallback.
    if let Some(rel) = fat32_relative_path(path) {
        if rel.is_empty() {
            return data_is_mounted();
        }
        if crate::fs::fat32::is_mounted() {
            let vol = crate::fs::fat32::FAT32_VOLUME.lock();
            if let Some(vol) = vol.as_ref() {
                return vol.lookup(rel).map(|e| e.is_dir()).unwrap_or(false);
            }
        }
    }
    false
}

/// Phase 31: Read a file's entire contents from disk filesystems (ext2, FAT32, tmpfs).
///
/// Used by `sys_execve` to load binaries from persistent storage instead of
/// only the ramdisk. Returns `Ok(contents)` on success or `Err(neg_errno)` on
/// failure (e.g. `NEG_ENOENT` if not found, `NEG_E2BIG` if too large).
const NEG_E2BIG: u64 = (-7_i64) as u64;

fn read_file_from_disk(path: &str) -> Result<alloc::vec::Vec<u8>, u64> {
    /// Maximum executable size we are willing to load (16 MB).
    const MAX_EXEC_SIZE: usize = 16 * 1024 * 1024;

    // Try ext2 root filesystem first (most likely location for compiled binaries).
    // Skip /data/ paths — those are routed to FAT32 by other syscalls.
    if crate::fs::ext2::is_mounted() && !path.starts_with("/data/") {
        let vol = crate::fs::ext2::EXT2_VOLUME.lock();
        if let Some(vol) = vol.as_ref() {
            let rel = path.trim_start_matches('/');
            if let Ok(inode_num) = vol.resolve_path(rel)
                && let Ok(inode) = vol.read_inode(inode_num)
            {
                let size = inode.size as usize;
                if size > MAX_EXEC_SIZE {
                    log::warn!(
                        "[exec] file too large ({} bytes > {} limit): {}",
                        size,
                        MAX_EXEC_SIZE,
                        path
                    );
                    return Err(NEG_E2BIG);
                }
                if size > 0 {
                    let mut buf = alloc::vec![0u8; size];
                    if let Ok(n) = vol.read_file_data(&inode, 0, &mut buf) {
                        buf.truncate(n);
                        return Ok(buf);
                    }
                }
            }
        }
    }

    // Try tmpfs (/tmp).
    if let Some(rel) = tmpfs_relative_path(path)
        && !rel.is_empty()
    {
        let tmpfs = crate::fs::tmpfs::TMPFS.lock();
        if let Ok(data) = tmpfs.read_file(rel, 0, usize::MAX)
            && !data.is_empty()
        {
            return Ok(data.to_vec());
        }
    }

    // Try FAT32 (/data mount).
    let fat_rel = if let Some(stripped) = path.strip_prefix("/data/") {
        Some(stripped)
    } else if path.starts_with("/usr/") {
        Some(path.trim_start_matches('/'))
    } else {
        None
    };
    if let Some(rel) = fat_rel {
        let vol = crate::fs::fat32::FAT32_VOLUME.lock();
        if let Some(vol) = vol.as_ref()
            && let Ok(entry) = vol.lookup(rel)
            && !entry.is_dir()
        {
            let size = entry.file_size as usize;
            if size > MAX_EXEC_SIZE {
                log::warn!(
                    "[exec] file too large ({} bytes > {} limit): {}",
                    size,
                    MAX_EXEC_SIZE,
                    path
                );
                return Err(NEG_E2BIG);
            }
            if size > 0 {
                let cluster = entry.start_cluster();
                let mut buf = alloc::vec![0u8; size];
                if let Ok(n) = vol.read_file(cluster, entry.file_size, 0, &mut buf)
                    && n == size
                {
                    return Ok(buf);
                }
            }
        }
    }

    Err(NEG_ENOENT)
}

/// Check if a path targets the tmpfs mount at `/tmp`.
///
/// Returns `Some(relative_path)` if so (e.g. "/tmp/foo" → "foo").
/// Rejects paths containing `.`, `..`, or empty segments to prevent
/// traversal outside the `/tmp` mount boundary.
fn tmpfs_relative_path(path: &str) -> Option<&str> {
    let trimmed = path.trim_start_matches('/');
    let rest = if trimmed == "tmp" {
        ""
    } else {
        trimmed.strip_prefix("tmp/")?
    };

    // For non-empty relative paths, reject `.`, `..`, and empty segments.
    if !rest.is_empty() {
        for segment in rest.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return None;
            }
        }
    }

    Some(rest)
}

/// Return the relative path within `/data` if this path starts with `/data`.
/// Kept for backwards compatibility with FAT32 fallback.
fn fat32_relative_path(path: &str) -> Option<&str> {
    let trimmed = path.trim_start_matches('/');
    let rest = if trimmed == "data" {
        ""
    } else {
        trimmed.strip_prefix("data/")?
    };

    if !rest.is_empty() {
        for segment in rest.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return None;
            }
        }
    }

    Some(rest)
}

/// Return the ext2 root-relative path for an absolute path.
///
/// When ext2 is mounted at `/`, every path is potentially on ext2.
/// Returns `None` only for paths claimed by tmpfs (`/tmp`) or that
/// fail traversal validation.
fn ext2_root_path(path: &str) -> Option<&str> {
    // /tmp is always tmpfs, never ext2
    if path == "/tmp" || path.starts_with("/tmp/") {
        return None;
    }

    let rest = path.strip_prefix('/').unwrap_or(path);

    // Reject `.`, `..`, and empty segments.
    if !rest.is_empty() {
        for segment in rest.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return None;
            }
        }
    }

    Some(rest)
}

fn open_resolved_path(name: &str, flags: u64, mode_arg: u64) -> u64 {
    // Decode POSIX access mode (O_ACCMODE = 0o3).
    let (readable, writable) = match flags & 0o3 {
        0 => (true, false),     // O_RDONLY
        1 => (false, true),     // O_WRONLY
        2 => (true, true),      // O_RDWR
        _ => return NEG_EINVAL, // invalid combination
    };

    // Phase 27: Permission check for existing files.
    let create = (flags & 0x40) != 0; // O_CREAT
    let file_meta = path_metadata(name);
    if (!create || file_meta.is_some())
        && let Some((fu, fg, fm)) = file_meta
    {
        let (_, _, euid, egid) = current_process_ids();
        let required = (if readable { 4u8 } else { 0 }) | (if writable { 2u8 } else { 0 });
        if required != 0 && !check_permission(fu, fg, fm, euid, egid, required) {
            return NEG_EACCES;
        }
    }

    // Phase 27: When creating a new file, check parent directory write+execute permission.
    if create
        && file_meta.is_none()
        && let Some((pu, pg, pm)) = parent_dir_metadata(name)
    {
        let (_, _, euid_c, egid_c) = current_process_ids();
        if !check_permission(pu, pg, pm, euid_c, egid_c, 3) {
            return NEG_EACCES;
        }
    }

    // Phase 21: /dev/null special file — reads return EOF, writes are discarded.
    // Phase 38: /dev/zero, /dev/urandom, /dev/full device nodes.
    // Placed after flags decode so O_RDONLY/O_WRONLY are respected.
    let dev_backend = match name {
        "/dev/null" => Some(FdBackend::DevNull),
        "/dev/zero" => Some(FdBackend::DevZero),
        "/dev/urandom" | "/dev/random" => Some(FdBackend::DevUrandom),
        "/dev/full" => Some(FdBackend::DevFull),
        _ => None,
    };
    if let Some(backend) = dev_backend {
        let entry = FdEntry {
            backend,
            offset: 0,
            readable,
            writable,
            cloexec: false,
            nonblock: false,
        };
        return match alloc_fd(3, entry) {
            Some(i) => i as u64,
            None => NEG_EMFILE,
        };
    }

    // Phase 29: /dev/ptmx — allocate a PTY pair and return the master fd.
    if name == "/dev/ptmx" {
        let pty_id = match crate::pty::alloc_pty() {
            Ok(id) => id,
            Err(()) => return NEG_ENOSPC,
        };
        log::info!("[pty] allocated PTY pair {}", pty_id);
        let entry = FdEntry {
            backend: FdBackend::PtyMaster { pty_id },
            offset: 0,
            readable: true,
            writable: true,
            cloexec: false,
            nonblock: false,
        };
        return match alloc_fd(3, entry) {
            Some(i) => i as u64,
            None => {
                crate::pty::close_master(pty_id);
                NEG_EMFILE
            }
        };
    }

    // Phase 29: /dev/pts/N — open the slave side of PTY N.
    if let Some(suffix) = name.strip_prefix("/dev/pts/") {
        if let Ok(pty_id) = suffix.parse::<u32>() {
            // Check + increment refcount under the same lock to prevent
            // a race where the PTY is freed between check and alloc_fd.
            {
                let mut table = crate::pty::PTY_TABLE.lock();
                match table.get_mut(pty_id as usize).and_then(|s| s.as_mut()) {
                    None => return NEG_ENOENT,
                    Some(pair) if pair.locked => return NEG_EIO,
                    Some(pair) => {
                        pair.slave_refcount += 1;
                        pair.slave_opened = true;
                    }
                }
            }
            let entry = FdEntry {
                backend: FdBackend::PtySlave { pty_id },
                offset: 0,
                readable: true,
                writable: true,
                cloexec: false,
                nonblock: false,
            };
            return match alloc_fd(3, entry) {
                Some(i) => i as u64,
                None => {
                    crate::pty::close_slave(pty_id);
                    NEG_EMFILE
                }
            };
        }
        return NEG_ENOENT;
    }

    // /dev/tty — resolve to the calling process's controlling terminal.
    if name == "/dev/tty" {
        let calling_pid = crate::process::current_pid();
        let ctty = {
            let pt = crate::process::PROCESS_TABLE.lock();
            pt.find(calling_pid).and_then(|p| p.controlling_tty.clone())
        };
        let (backend, maybe_pty_id) = match ctty {
            Some(crate::process::ControllingTty::Console) => {
                (FdBackend::DeviceTTY { tty_id: 0 }, None)
            }
            Some(crate::process::ControllingTty::Pty(id)) => {
                let mut table = crate::pty::PTY_TABLE.lock();
                match table.get_mut(id as usize).and_then(|s| s.as_mut()) {
                    None => return NEG_ENXIO,
                    Some(pair) => {
                        pair.slave_refcount += 1;
                    }
                }
                (FdBackend::PtySlave { pty_id: id }, Some(id))
            }
            None => return NEG_ENXIO,
        };
        let entry = FdEntry {
            backend,
            offset: 0,
            readable,
            writable,
            cloexec: false,
            nonblock: false,
        };
        return match alloc_fd(3, entry) {
            Some(i) => i as u64,
            None => {
                if let Some(id) = maybe_pty_id {
                    crate::pty::close_slave(id);
                }
                NEG_EMFILE
            }
        };
    }

    let create = flags & O_CREAT != 0;
    let truncate = flags & O_TRUNC != 0;
    let append = flags & O_APPEND != 0;

    // Handle directory opens (Phase 18).
    let o_directory = flags & O_DIRECTORY != 0;
    let path_is_dir = is_directory(name);

    if o_directory && !path_is_dir {
        // O_DIRECTORY set on a non-directory (or non-existent path).
        // Check if the path exists as a file — if so, ENOTDIR.
        if let Some(rel) = tmpfs_relative_path(name) {
            if !rel.is_empty() {
                let tmpfs = crate::fs::tmpfs::TMPFS.lock();
                if tmpfs.stat(rel).is_ok() {
                    return NEG_ENOTDIR;
                }
            }
        } else if crate::fs::ramdisk::get_file(name).is_some() {
            return NEG_ENOTDIR;
        }
        // Path doesn't exist — fall through to normal open which will return ENOENT.
    }

    if path_is_dir {
        // Directories cannot be opened for writing, creation, or truncation.
        if writable || create || truncate {
            return NEG_EISDIR;
        }
        let entry = FdEntry {
            backend: FdBackend::Dir {
                path: alloc::string::String::from(name),
            },
            offset: 0,
            readable: true,
            writable: false,
            cloexec: false,
            nonblock: false,
        };
        return match alloc_fd(3, entry) {
            Some(i) => {
                log::debug!("[open] {} → fd {} (dir)", name, i);
                i as u64
            }
            None => NEG_EMFILE,
        };
    }

    if crate::fs::procfs::path_exists(name) {
        if writable || create || truncate || append {
            return NEG_EROFS;
        }
        if !matches!(
            crate::fs::procfs::path_node(name),
            Some(crate::fs::procfs::ProcfsNode::File)
        ) {
            return NEG_ENOENT;
        }
        let entry = FdEntry {
            backend: FdBackend::Proc {
                path: alloc::string::String::from(name),
                snapshot: (name == "/proc/kmsg").then(crate::fs::procfs::render_kmsg_bytes),
            },
            offset: 0,
            readable: true,
            writable: false,
            cloexec: false,
            nonblock: false,
        };
        return match alloc_fd(3, entry) {
            Some(i) => i as u64,
            None => NEG_EMFILE,
        };
    }

    // Check if this is a tmpfs path.
    if let Some(rel) = tmpfs_relative_path(name) {
        if rel.is_empty() {
            // /tmp itself handled as directory above; shouldn't reach here.
            return NEG_EISDIR;
        }

        let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();

        // Open or create the file with caller's ownership.
        let create_mode = ((mode_arg as u16) & 0o7777) & !current_umask();
        let (_, _, caller_euid, caller_egid) = current_process_ids();
        match tmpfs.open_or_create_with_meta(rel, create, caller_euid, caller_egid, create_mode) {
            Ok(_created) => {}
            Err(crate::fs::tmpfs::TmpfsError::NotFound) => return NEG_ENOENT,
            Err(crate::fs::tmpfs::TmpfsError::WrongType) => {
                return NEG_EISDIR;
            }
            Err(crate::fs::tmpfs::TmpfsError::NotADirectory) => {
                return NEG_ENOTDIR;
            }
            Err(_) => return NEG_EINVAL,
        }

        if truncate && writable {
            let _ = tmpfs.truncate(rel, 0);
        }

        let initial_offset = if append {
            tmpfs.file_size(rel).unwrap_or(0)
        } else {
            0
        };

        drop(tmpfs);

        // Allocate an fd slot in the current process's table.
        let entry = FdEntry {
            backend: FdBackend::Tmpfs {
                path: alloc::string::String::from(rel),
            },
            offset: initial_offset,
            readable,
            writable,
            cloexec: false,
            nonblock: false,
        };
        match alloc_fd(3, entry) {
            Some(i) => {
                log::debug!("[open] {} → fd {} (tmpfs)", name, i);
                return i as u64;
            }
            None => {
                log::warn!("[open] fd table full");
                return NEG_EMFILE;
            }
        }
    }

    // Phase 24/28: check if this is a /data path (ext2 or FAT32).
    if let Some(rel) = fat32_relative_path(name) {
        if crate::fs::ext2::is_mounted() {
            return open_ext2_file(
                name, rel, readable, writable, create, append, truncate, mode_arg,
            );
        }
        if data_is_mounted() {
            if rel.is_empty() {
                return NEG_EISDIR;
            }
            let mut vol_guard = crate::fs::fat32::FAT32_VOLUME.lock();
            if let Some(vol) = vol_guard.as_mut() {
                match vol.lookup(rel) {
                    Ok(entry) => {
                        if entry.is_dir() {
                            if writable || create || truncate {
                                return NEG_EISDIR;
                            }
                            let fd_entry = FdEntry {
                                backend: FdBackend::Dir {
                                    path: alloc::string::String::from(name),
                                },
                                offset: 0,
                                readable: true,
                                writable: false,
                                cloexec: false,
                                nonblock: false,
                            };

                            return match alloc_fd(3, fd_entry) {
                                Some(i) => {
                                    log::debug!("[open] {} → fd {} (fat32 dir)", name, i);
                                    i as u64
                                }
                                None => NEG_EMFILE,
                            };
                        }

                        // Find parent dir cluster for writes.
                        let parts: alloc::vec::Vec<&str> =
                            rel.split('/').filter(|s| !s.is_empty()).collect();
                        let parent_cluster = if parts.len() <= 1 {
                            vol.bpb.root_cluster
                        } else {
                            let parent_path = parts[..parts.len() - 1].join("/");
                            match vol.lookup(&parent_path) {
                                Ok(pe) if pe.is_dir() => pe.start_cluster(),
                                Ok(_) => return NEG_ENOTDIR,
                                Err(_) => return NEG_ENOENT,
                            }
                        };

                        let initial_offset = if append { entry.file_size as usize } else { 0 };

                        let mut fd_entry = FdEntry {
                            backend: FdBackend::Fat32Disk {
                                path: alloc::string::String::from(rel),
                                start_cluster: entry.start_cluster(),
                                file_size: entry.file_size,
                                dir_cluster: parent_cluster,
                            },
                            offset: initial_offset,
                            readable,
                            writable,
                            cloexec: false,
                            nonblock: false,
                        };

                        // Phase 31: support O_TRUNC on FAT32 — free the old
                        // cluster chain and reset size to 0 so TCC can overwrite
                        // output files.
                        if truncate && writable {
                            let old_cluster = entry.start_cluster();
                            if old_cluster >= 2 && vol.free_chain(old_cluster).is_err() {
                                return NEG_EIO;
                            }
                            let file_short = rel.rsplit('/').next().unwrap_or(rel);
                            if vol
                                .update_dir_entry(parent_cluster, file_short, 0, 0)
                                .is_err()
                            {
                                return NEG_EIO;
                            }
                            fd_entry.backend = FdBackend::Fat32Disk {
                                path: alloc::string::String::from(rel),
                                start_cluster: 0,
                                file_size: 0,
                                dir_cluster: parent_cluster,
                            };
                            fd_entry.offset = 0;
                        }

                        return match alloc_fd(3, fd_entry) {
                            Some(i) => {
                                log::debug!("[open] {} → fd {} (fat32)", name, i);
                                i as u64
                            }
                            None => NEG_EMFILE,
                        };
                    }
                    Err(kernel_core::fs::fat32::Fat32Error::NotFound) if create => {
                        // Create a new file (same lock guard, no deadlock).
                        let parts: alloc::vec::Vec<&str> =
                            rel.split('/').filter(|s| !s.is_empty()).collect();
                        let (parent_cluster, file_name) = if parts.len() <= 1 {
                            (vol.bpb.root_cluster, rel)
                        } else {
                            let parent_path = parts[..parts.len() - 1].join("/");
                            let parent_cluster = match vol.lookup(&parent_path) {
                                Ok(pe) if pe.is_dir() => pe.start_cluster(),
                                _ => return NEG_ENOENT,
                            };
                            (parent_cluster, parts[parts.len() - 1])
                        };

                        match vol.create_file(parent_cluster, file_name) {
                            Ok(_entry) => {
                                let fd_entry = FdEntry {
                                    backend: FdBackend::Fat32Disk {
                                        path: alloc::string::String::from(rel),
                                        start_cluster: 0,
                                        file_size: 0,
                                        dir_cluster: parent_cluster,
                                    },
                                    offset: 0,
                                    readable,
                                    writable,
                                    cloexec: false,
                                    nonblock: false,
                                };

                                // Set ownership and permissions on the newly created file.
                                let create_mode = ((mode_arg as u16) & 0o7777) & !current_umask();
                                let (_, _, caller_euid, caller_egid) = current_process_ids();
                                crate::fs::fat32::set_fat32_meta(
                                    rel,
                                    caller_euid,
                                    caller_egid,
                                    create_mode,
                                );

                                return match alloc_fd(3, fd_entry) {
                                    Some(i) => {
                                        log::debug!("[open] {} → fd {} (fat32 new)", name, i);
                                        i as u64
                                    }
                                    None => NEG_EMFILE,
                                };
                            }
                            Err(_) => return NEG_EIO,
                        }
                    }
                    Err(_) => return NEG_ENOENT,
                }
            }
        } else {
            // FAT32 not mounted — /data doesn't exist.
            return NEG_ENOENT;
        }
    }

    // Phase 28: ext2 root filesystem — try before ramdisk for non-/bin, non-/sbin.
    if crate::fs::ext2::is_mounted()
        && let Some(rel) = ext2_root_path(name)
    {
        // Check if ramdisk has this path (e.g. /bin/cat) — ramdisk takes priority.
        if crate::fs::ramdisk::ramdisk_lookup(name).is_none() {
            return open_ext2_file(
                name, rel, readable, writable, create, append, truncate, mode_arg,
            );
        }
    }

    // Fall through to ramdisk lookup — ramdisk is read-only.
    if writable || create {
        // If ext2 is mounted, try creating there before giving up.
        if crate::fs::ext2::is_mounted()
            && let Some(rel) = ext2_root_path(name)
        {
            return open_ext2_file(
                name, rel, readable, writable, create, append, truncate, mode_arg,
            );
        }
        return NEG_EROFS;
    }

    let content = match crate::fs::ramdisk::get_file(name) {
        Some(c) => c,
        None => {
            // Try ext2 root for anything ramdisk doesn't have.
            if crate::fs::ext2::is_mounted()
                && let Some(rel) = ext2_root_path(name)
            {
                return open_ext2_file(
                    name, rel, readable, writable, create, append, truncate, mode_arg,
                );
            }
            // Legacy: /etc/* fallback — try /data/etc/* on FAT32 only.
            if let Some(etc_rel) = name.strip_prefix("/etc/")
                && !etc_rel.is_empty()
                && crate::fs::fat32::is_mounted()
            {
                let data_rel = alloc::format!("etc/{}", etc_rel);
                let vol = crate::fs::fat32::FAT32_VOLUME.lock();
                if let Some(vol) = vol.as_ref()
                    && let Ok(entry) = vol.lookup(&data_rel)
                    && !entry.is_dir()
                {
                    let fd_entry = FdEntry {
                        backend: FdBackend::Fat32Disk {
                            path: data_rel,
                            start_cluster: entry.start_cluster(),
                            file_size: entry.file_size,
                            dir_cluster: vol.bpb.root_cluster,
                        },
                        offset: 0,
                        readable: true,
                        writable: false,
                        cloexec: false,
                        nonblock: false,
                    };
                    return match alloc_fd(3, fd_entry) {
                        Some(i) => {
                            log::debug!("[open] {} → fd {} (fat32 /etc alias)", name, i);
                            i as u64
                        }
                        None => NEG_EMFILE,
                    };
                }
            }
            log::warn!("[open] file not found: {}", name);
            return NEG_ENOENT;
        }
    };

    let entry = FdEntry {
        backend: FdBackend::Ramdisk {
            content_addr: content.as_ptr() as usize,
            content_len: content.len(),
        },
        offset: 0,
        readable: true,
        writable: false,
        cloexec: false,
        nonblock: false,
    };
    match alloc_fd(3, entry) {
        Some(i) => {
            log::debug!("[open] {} → fd {}", name, i);
            i as u64
        }
        None => {
            log::warn!("[open] fd table full");
            NEG_EMFILE
        }
    }
}

fn sys_linux_open(path_ptr: u64, flags: u64, mode_arg: u64) -> u64 {
    let mut buf = [0u8; 512];
    let raw_name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    open_user_path(AT_FDCWD, raw_name, flags, mode_arg)
}

// ---------------------------------------------------------------------------
// Phase 18: openat(dirfd, path, flags, mode) — syscall 257
// ---------------------------------------------------------------------------

fn sys_linux_openat(dirfd: u64, path_ptr: u64, flags: u64) -> u64 {
    // Read mode from SYSCALL_ARG3 (r10 — 4th syscall argument in Linux ABI).
    let mode_arg = per_core_syscall_arg3();
    let mut buf = [0u8; 512];
    let rel_name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    open_user_path(dirfd, rel_name, flags, mode_arg)
}

pub(crate) fn cleanup_ext2_inode_if_unused(inode_num: u32) {
    let mut vol = crate::fs::ext2::EXT2_VOLUME.lock();
    let Some(vol) = vol.as_mut() else {
        return;
    };
    let Ok(mut inode) = vol.read_inode(inode_num) else {
        return;
    };
    if inode.links_count != 0 {
        return;
    }
    if crate::process::ext2_inode_open_count(inode_num) != 0 {
        return;
    }
    let _ = vol.truncate_file(inode_num, &mut inode);
    let _ = vol.free_inode(inode_num);
}

// ---------------------------------------------------------------------------
// T015 (close) / T013 (close)
// ---------------------------------------------------------------------------

fn sys_linux_close(fd: u64) -> u64 {
    let fd = fd as usize;
    // stdin/stdout/stderr (0–2) are virtual and cannot be closed.
    if fd < 3 {
        return 0;
    }
    if fd >= MAX_FDS {
        return NEG_EBADF;
    }
    let mut ext2_inode = None;
    // Close-time cleanup for resource-backed FDs.
    if let Some(entry) = current_fd_entry(fd) {
        match &entry.backend {
            FdBackend::PipeRead { pipe_id } => crate::pipe::pipe_close_reader(*pipe_id),
            FdBackend::PipeWrite { pipe_id } => crate::pipe::pipe_close_writer(*pipe_id),
            FdBackend::Socket { handle } => crate::net::free_socket(*handle),
            FdBackend::UnixSocket { handle } => crate::net::unix::free_unix_socket(*handle),
            FdBackend::PtyMaster { pty_id } => crate::pty::close_master(*pty_id),
            FdBackend::PtySlave { pty_id } => crate::pty::close_slave(*pty_id),
            FdBackend::Epoll { instance_id } => epoll_free(*instance_id),
            FdBackend::Ext2Disk { inode_num, .. } => ext2_inode = Some(*inode_num),
            _ => {}
        }
    }
    // Remove this FD from all epoll interest lists to prevent stale references.
    epoll_remove_fd(fd);
    let mut found = false;
    with_current_fd_mut(fd, |slot| {
        if slot.is_some() {
            *slot = None;
            found = true;
        }
    });
    if !found {
        return NEG_EBADF;
    }
    if let Some(inode_num) = ext2_inode {
        cleanup_ext2_inode_if_unused(inode_num);
    }
    0
}

// ---------------------------------------------------------------------------
// T016: fstat(fd, stat_ptr)
// ---------------------------------------------------------------------------

/// Write a minimal Linux x86_64 `stat` struct to `stat_ptr`.
///
/// Only `st_size` (offset 48) and `st_mode` (offset 24) are filled in;
/// all other fields are zero.  This satisfies musl's `fstat` use in `fopen`.
/// Get uid/gid/mode for a directory path from the appropriate filesystem.
fn dir_metadata(path: &str) -> (u32, u32, u16) {
    // Tmpfs directories (under /tmp)
    if path.starts_with("/tmp") || path == "tmp" {
        let rel = path.strip_prefix("/tmp").unwrap_or(path);
        let lookup = if rel.is_empty() { "/" } else { rel };
        let tmpfs = crate::fs::tmpfs::TMPFS.lock();
        if let Ok(s) = tmpfs.stat(lookup) {
            return (s.uid, s.gid, s.mode);
        }
    }
    // ext2 root filesystem directories.
    if crate::fs::ext2::is_mounted()
        && let Some(rel) = ext2_root_path(path)
    {
        return data_file_metadata(rel).unwrap_or((0, 0, 0o755));
    }
    // Legacy: /data paths for FAT32 fallback.
    if let Some(rel) = path.strip_prefix("/data/") {
        return data_file_metadata(rel).unwrap_or((0, 0, 0o755));
    }
    // Default for ramdisk and other directories
    (0, 0, 0o755)
}

/// Get uid/gid/mode for a file on the data partition (ext2 or FAT32).
/// Returns `None` if the file is not found or the volume is not mounted.
fn data_file_metadata(rel: &str) -> Option<(u32, u32, u16)> {
    if crate::fs::ext2::is_mounted() {
        return crate::fs::ext2::get_ext2_meta(rel);
    }
    Some(crate::fs::fat32::get_fat32_meta(rel))
}

/// Set permission mode on a data partition file (ext2 or FAT32).
/// Returns 0 on success, NEG_ENOENT if not found, NEG_EIO on error.
fn data_chmod(rel: &str, mode: u16) -> u64 {
    if crate::fs::ext2::is_mounted() {
        let mut vol = crate::fs::ext2::EXT2_VOLUME.lock();
        let vol = match vol.as_mut() {
            Some(v) => v,
            None => return NEG_EIO,
        };
        let (u, g, _, _, _) = match vol.metadata(rel) {
            Ok(m) => m,
            Err(_) => return NEG_ENOENT,
        };
        match vol.set_metadata(rel, u, g, mode) {
            Ok(()) => 0,
            Err(_) => NEG_EIO,
        }
    } else {
        let (u, g, _) = crate::fs::fat32::get_fat32_meta(rel);
        crate::fs::fat32::set_fat32_meta_and_save(rel, u, g, mode);
        0
    }
}

/// Set ownership on a data partition file (ext2 or FAT32).
/// Returns 0 on success, NEG_ENOENT if not found, NEG_EIO on error.
fn data_chown(rel: &str, new_uid: u32, new_gid: u32) -> u64 {
    if crate::fs::ext2::is_mounted() {
        let mut vol = crate::fs::ext2::EXT2_VOLUME.lock();
        let vol = match vol.as_mut() {
            Some(v) => v,
            None => return NEG_EIO,
        };
        let (_, _, mode, _, _) = match vol.metadata(rel) {
            Ok(m) => m,
            Err(_) => return NEG_ENOENT,
        };
        match vol.set_metadata(rel, new_uid, new_gid, mode & 0o7777) {
            Ok(()) => 0,
            Err(_) => NEG_EIO,
        }
    } else {
        let (_, _, m) = crate::fs::fat32::get_fat32_meta(rel);
        crate::fs::fat32::set_fat32_meta_and_save(rel, new_uid, new_gid, m);
        0
    }
}

/// Open a file on the ext2 partition.
#[allow(clippy::too_many_arguments)]
fn open_ext2_file(
    name: &str,
    rel: &str,
    readable: bool,
    writable: bool,
    create: bool,
    append: bool,
    truncate: bool,
    mode_arg: u64,
) -> u64 {
    const NEG_EISDIR: u64 = (-21_i64) as u64;
    const NEG_ENOENT: u64 = (-2_i64) as u64;
    const NEG_EMFILE: u64 = (-24_i64) as u64;
    const NEG_EIO: u64 = (-5_i64) as u64;

    if rel.is_empty() {
        return NEG_EISDIR;
    }

    let mut vol = crate::fs::ext2::EXT2_VOLUME.lock();
    let vol = match vol.as_mut() {
        Some(v) => v,
        None => return NEG_EIO,
    };

    match vol.resolve_path(rel) {
        Ok(ino) => {
            let inode = match vol.read_inode(ino) {
                Ok(i) => i,
                Err(_) => return NEG_EIO,
            };

            if inode.is_dir() {
                if writable || create || truncate {
                    return NEG_EISDIR;
                }
                let fd_entry = FdEntry {
                    backend: FdBackend::Dir {
                        path: alloc::string::String::from(name),
                    },
                    offset: 0,
                    readable: true,
                    writable: false,
                    cloexec: false,
                    nonblock: false,
                };
                return match alloc_fd(3, fd_entry) {
                    Some(i) => i as u64,
                    None => NEG_EMFILE,
                };
            }

            // Truncate if requested.
            let mut inode = inode;
            if truncate && writable && vol.truncate_file(ino, &mut inode).is_err() {
                return NEG_EIO;
            }

            let initial_offset = if append { inode.size as usize } else { 0 };

            // Find parent inode for writes.
            let parent_ino = {
                let parts: alloc::vec::Vec<&str> =
                    rel.split('/').filter(|s| !s.is_empty()).collect();
                if parts.len() <= 1 {
                    kernel_core::fs::ext2::EXT2_ROOT_INO
                } else {
                    let parent_path = parts[..parts.len() - 1].join("/");
                    match vol.resolve_path(&parent_path) {
                        Ok(p) => p,
                        Err(_) => return NEG_ENOENT,
                    }
                }
            };

            let fd_entry = FdEntry {
                backend: FdBackend::Ext2Disk {
                    path: alloc::string::String::from(rel),
                    inode_num: ino,
                    file_size: inode.size,
                    parent_inode: parent_ino,
                },
                offset: initial_offset,
                readable,
                writable,
                cloexec: false,
                nonblock: false,
            };

            match alloc_fd(3, fd_entry) {
                Some(i) => {
                    log::debug!("[open] {} → fd {} (ext2)", name, i);
                    i as u64
                }
                None => NEG_EMFILE,
            }
        }
        Err(kernel_core::fs::ext2::Ext2Error::NotFound) if create => {
            // Create a new file.
            let parts: alloc::vec::Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
            let (parent_ino, file_name) = if parts.len() <= 1 {
                (kernel_core::fs::ext2::EXT2_ROOT_INO, rel)
            } else {
                let parent_path = parts[..parts.len() - 1].join("/");
                let parent_ino = match vol.resolve_path(&parent_path) {
                    Ok(p) => p,
                    Err(_) => return NEG_ENOENT,
                };
                (parent_ino, parts[parts.len() - 1])
            };

            let create_mode = ((mode_arg as u16) & 0o7777) & !current_umask();
            let (_, _, caller_euid, caller_egid) = current_process_ids();

            match vol.create_file(parent_ino, file_name, create_mode, caller_euid, caller_egid) {
                Ok(new_ino) => {
                    let fd_entry = FdEntry {
                        backend: FdBackend::Ext2Disk {
                            path: alloc::string::String::from(rel),
                            inode_num: new_ino,
                            file_size: 0,
                            parent_inode: parent_ino,
                        },
                        offset: 0,
                        readable,
                        writable,
                        cloexec: false,
                        nonblock: false,
                    };
                    match alloc_fd(3, fd_entry) {
                        Some(i) => {
                            log::debug!("[open] {} → fd {} (ext2 new)", name, i);
                            i as u64
                        }
                        None => NEG_EMFILE,
                    }
                }
                Err(_) => NEG_EIO,
            }
        }
        Err(_) => NEG_ENOENT,
    }
}

/// Check if the data partition is mounted (ext2 or FAT32).
fn data_is_mounted() -> bool {
    crate::fs::ext2::is_mounted() || crate::fs::fat32::is_mounted()
}

fn sys_linux_fstat(fd: u64, stat_ptr: u64) -> u64 {
    let fd_idx = fd as usize;
    if fd_idx >= MAX_FDS {
        return NEG_EBADF;
    }
    let entry = match current_fd_entry(fd_idx) {
        Some(e) => e,
        None => return NEG_EBADF,
    };
    // x86_64 stat struct layout (144 bytes):
    //  0: st_dev (u64)      8: st_ino (u64)    16: st_nlink (u64)
    // 24: st_mode (u32)    28: st_uid (u32)    32: st_gid (u32)
    // 36: __pad0 (u32)     40: st_rdev (u64)   48: st_size (i64)
    // 56: st_blksize (i64) 64: st_blocks (i64)
    let mut stat = [0u8; 144];
    let blksize: u64 = 4096;

    // Determine mode, uid, gid, size, rdev based on backend type.
    let (mode, uid, gid, size, rdev): (u32, u32, u32, u64, u64) = match &entry.backend {
        FdBackend::Dir { path } => {
            if let Some(st) = crate::fs::procfs::stat(path) {
                stat[8..16].copy_from_slice(&st.ino.to_ne_bytes());
                stat[16..24].copy_from_slice(&st.nlink.to_ne_bytes());
                (st.mode, st.uid, st.gid, st.size, 0)
            } else if let Some(rel) = tmpfs_relative_path(path) {
                let tmpfs = crate::fs::tmpfs::TMPFS.lock();
                match tmpfs.stat(rel) {
                    Ok(st) => {
                        stat[8..16].copy_from_slice(&st.ino.to_ne_bytes());
                        stat[16..24].copy_from_slice(&st.nlink.to_ne_bytes());
                        (0x4000 | st.mode as u32, st.uid, st.gid, st.size as u64, 0)
                    }
                    Err(_) => return NEG_ENOENT,
                }
            } else {
                let (u, g, m) = dir_metadata(path);
                (0x4000 | m as u32, u, g, 0, 0)
            }
        }
        FdBackend::DevNull | FdBackend::DevZero | FdBackend::DevUrandom | FdBackend::DevFull => {
            (0x2000 | 0o666, 0, 0, 0, 0)
        }
        FdBackend::Proc { path, .. } => {
            let Some(st) = crate::fs::procfs::stat(path) else {
                return NEG_ENOENT;
            };
            stat[8..16].copy_from_slice(&st.ino.to_ne_bytes());
            stat[16..24].copy_from_slice(&st.nlink.to_ne_bytes());
            (st.mode, st.uid, st.gid, st.size, 0)
        }
        FdBackend::DeviceTTY { tty_id } => {
            (0x2000 | 0o620, 0, 0, 0, ((5u64) << 8) | (*tty_id as u64))
        }
        FdBackend::PtyMaster { pty_id } => (
            0x2000 | 0o620,
            0,
            0,
            0,
            ((5u64) << 8) | (2 + *pty_id as u64),
        ),
        FdBackend::PtySlave { pty_id } => {
            (0x2000 | 0o620, 0, 0, 0, ((136u64) << 8) | (*pty_id as u64))
        }
        FdBackend::Socket { .. } | FdBackend::UnixSocket { .. } => (0xC000 | 0o755, 0, 0, 0, 0),
        FdBackend::Stdout | FdBackend::Stdin => (0x2000 | 0o620, 0, 0, 0, 0),
        FdBackend::PipeRead { .. } | FdBackend::PipeWrite { .. } => (0x1000 | 0o600, 0, 0, 0, 0),
        FdBackend::Ramdisk { content_len, .. } => {
            // Ramdisk files: root-owned, mode 0o755 (all files, including non-executables)
            (0x8000 | 0o755, 0, 0, *content_len as u64, 0)
        }
        FdBackend::Tmpfs { path } => {
            let tmpfs = crate::fs::tmpfs::TMPFS.lock();
            match tmpfs.stat(path) {
                Ok(s) => {
                    stat[8..16].copy_from_slice(&s.ino.to_ne_bytes());
                    stat[16..24].copy_from_slice(&s.nlink.to_ne_bytes());
                    let mode = if s.is_symlink {
                        0xA000 | 0o777
                    } else if s.is_dir {
                        0x4000 | s.mode as u32
                    } else {
                        0x8000 | s.mode as u32
                    };
                    (mode, s.uid, s.gid, s.size as u64, 0)
                }
                Err(_) => return NEG_ENOENT,
            }
        }
        FdBackend::Fat32Disk {
            path, file_size, ..
        } => {
            let (u, g, m) = data_file_metadata(path).unwrap_or((0, 0, 0o755));
            (0x8000 | m as u32, u, g, *file_size as u64, 0)
        }
        FdBackend::Ext2Disk {
            inode_num,
            file_size,
            ..
        } => {
            // Phase 32: read inode to get timestamps and real metadata.
            let inode_num = *inode_num;
            let vol = crate::fs::ext2::EXT2_VOLUME.lock();
            if let Some(vol) = vol.as_ref()
                && let Ok(inode) = vol.read_inode(inode_num)
            {
                let mode = inode.mode as u32;
                let uid = inode.uid as u32;
                let gid = inode.gid as u32;
                let size = inode.size as u64; // use inode size, not cached FD size
                let nlink = inode.links_count as u64;
                let blk = vol.block_size as u64;
                let ino = inode_num as u64;
                stat[8..16].copy_from_slice(&ino.to_ne_bytes());
                stat[16..24].copy_from_slice(&nlink.to_ne_bytes());
                stat[24..28].copy_from_slice(&mode.to_ne_bytes());
                stat[28..32].copy_from_slice(&uid.to_ne_bytes());
                stat[32..36].copy_from_slice(&gid.to_ne_bytes());
                stat[48..56].copy_from_slice(&size.to_ne_bytes());
                stat[56..64].copy_from_slice(&blk.to_ne_bytes());
                let atime = inode.atime as i64;
                let mtime = inode.mtime as i64;
                let ctime = inode.ctime as i64;
                stat[72..80].copy_from_slice(&atime.to_ne_bytes());
                stat[88..96].copy_from_slice(&mtime.to_ne_bytes());
                stat[104..112].copy_from_slice(&ctime.to_ne_bytes());
                if crate::mm::user_mem::copy_to_user(stat_ptr, &stat).is_err() {
                    return NEG_EFAULT;
                }
                return 0;
            }
            // Fallback if inode read fails — use cached FD size
            let fallback_size = *file_size as u64;
            let (u, g, m) = (0u32, 0u32, 0o755u16);
            (0x8000 | m as u32, u, g, fallback_size, 0)
        }
        FdBackend::Epoll { .. } => (0x2000 | 0o600, 0, 0, 0, 0),
    };

    stat[24..28].copy_from_slice(&mode.to_ne_bytes());
    stat[28..32].copy_from_slice(&uid.to_ne_bytes());
    stat[32..36].copy_from_slice(&gid.to_ne_bytes());
    stat[40..48].copy_from_slice(&rdev.to_ne_bytes());
    stat[48..56].copy_from_slice(&size.to_ne_bytes());
    stat[56..64].copy_from_slice(&blksize.to_ne_bytes());

    if crate::mm::user_mem::copy_to_user(stat_ptr, &stat).is_err() {
        return NEG_EFAULT;
    }
    0
}

fn sys_statfs(path_ptr: u64, buf_ptr: u64) -> u64 {
    let mut buf = [0u8; 512];
    let raw = match read_user_cstr(path_ptr, &mut buf) {
        Some(p) => p,
        None => return NEG_EFAULT,
    };
    let lexical = match resolve_path_from_dirfd(AT_FDCWD, raw) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let abs = match resolve_existing_fs_path(&lexical, true) {
        Ok(path) => path,
        Err(err) => return err,
    };
    if !statfs_path_exists(&abs) {
        return NEG_ENOENT;
    }
    let stat = statfs_for_path(&abs);
    write_statfs_to_user(buf_ptr, &stat)
}

fn sys_fstatfs(fd: u64, buf_ptr: u64) -> u64 {
    let fd_idx = fd as usize;
    if fd_idx >= MAX_FDS {
        return NEG_EBADF;
    }
    let entry = match current_fd_entry(fd_idx) {
        Some(e) => e,
        None => return NEG_EBADF,
    };

    let stat = match &entry.backend {
        FdBackend::Tmpfs { .. } => tmpfs_statfs(),
        FdBackend::Proc { .. } => proc_statfs(),
        FdBackend::Fat32Disk { .. } => fat32_statfs(),
        FdBackend::Ext2Disk { .. } => ext2_statfs(),
        FdBackend::Ramdisk { .. } => ramdisk_statfs(),
        FdBackend::Dir { path } => statfs_for_path(path),
        FdBackend::DevNull
        | FdBackend::DevZero
        | FdBackend::DevUrandom
        | FdBackend::DevFull
        | FdBackend::DeviceTTY { .. }
        | FdBackend::PtyMaster { .. }
        | FdBackend::PtySlave { .. }
        | FdBackend::Epoll { .. } => ramdisk_statfs(),
        FdBackend::Stdin | FdBackend::Stdout => ramdisk_statfs(),
        FdBackend::PipeRead { .. } | FdBackend::PipeWrite { .. } => pipefs_statfs(),
        FdBackend::Socket { .. } | FdBackend::UnixSocket { .. } => sockfs_statfs(),
    };
    write_statfs_to_user(buf_ptr, &stat)
}

// ---------------------------------------------------------------------------
// Phase 27: chmod, fchmod, chown, fchown
// ---------------------------------------------------------------------------

const NEG_EACCES: u64 = (-13_i64) as u64;

// ---------------------------------------------------------------------------
// Phase 27 Track C: Permission enforcement
// ---------------------------------------------------------------------------

/// Check if a caller has the required permission on a file/directory.
///
/// `required` is a bitmask: 4=read, 2=write, 1=execute.
/// Returns true if access is allowed.
fn check_permission(
    file_uid: u32,
    file_gid: u32,
    file_mode: u16,
    caller_uid: u32,
    caller_gid: u32,
    required: u8,
) -> bool {
    // Root bypasses all permission checks.
    if caller_uid == 0 {
        return true;
    }

    let bits = if caller_uid == file_uid {
        ((file_mode >> 6) & 0o7) as u8
    } else if caller_gid == file_gid {
        ((file_mode >> 3) & 0o7) as u8
    } else {
        (file_mode & 0o7) as u8
    };

    (bits & required) == required
}

/// Get file metadata for permission checking on a resolved absolute path.
fn path_metadata(abs_path: &str) -> Option<(u32, u32, u16)> {
    if let Some(st) = crate::fs::procfs::stat(abs_path) {
        return Some((st.uid, st.gid, (st.mode & 0o7777) as u16));
    }
    if let Some(rel) = tmpfs_relative_path(abs_path) {
        let tmpfs = crate::fs::tmpfs::TMPFS.lock();
        if let Ok(s) = tmpfs.stat(rel) {
            return Some((s.uid, s.gid, s.mode));
        }
        return None;
    }
    // Ramdisk files (/bin/*, /sbin/*) are root-owned, 0o755.
    if crate::fs::ramdisk::ramdisk_lookup(abs_path).is_some() {
        return Some((0, 0, 0o755));
    }
    if abs_path == "/dev" || abs_path == "/dev/pts" {
        return Some((0, 0, 0o755));
    }
    if abs_path == "/dev/null"
        || abs_path == "/dev/zero"
        || abs_path == "/dev/urandom"
        || abs_path == "/dev/random"
        || abs_path == "/dev/full"
        || abs_path == "/dev/tty"
        || abs_path == "/dev/ptmx"
        || abs_path.starts_with("/dev/pts/")
    {
        return Some((0, 0, 0o666));
    }
    if abs_path == "/" || abs_path == "/tmp" {
        return Some((0, 0, 0o755));
    }
    // ext2 root filesystem — check for any path.
    if let Some(rel) = ext2_root_path(abs_path)
        && crate::fs::ext2::is_mounted()
    {
        return data_file_metadata(rel);
    }
    // Legacy: /data paths for FAT32 fallback.
    if let Some(rel) = abs_path.strip_prefix("/data/") {
        return data_file_metadata(rel);
    }
    None
}

/// Get metadata for the parent directory of a path.
fn parent_dir_metadata(abs_path: &str) -> Option<(u32, u32, u16)> {
    let trimmed = abs_path.trim_end_matches('/');
    if let Some(pos) = trimmed.rfind('/') {
        let parent = if pos == 0 { "/" } else { &trimmed[..pos] };
        path_metadata(parent)
    } else {
        path_metadata("/")
    }
}

/// Helper to resolve a path and apply a metadata-changing operation.
/// Returns the filesystem-relative path and which FS it belongs to.
enum FsTarget {
    Tmpfs(alloc::string::String),
    /// ext2 root (or FAT32 /data fallback). The string is the root-relative path.
    DiskData(alloc::string::String),
    Ramdisk,
}

fn resolve_fs_target(abs_path: &str) -> FsTarget {
    if abs_path.starts_with("/tmp/") || abs_path == "/tmp" {
        let rel = abs_path.strip_prefix("/tmp").unwrap_or("/");
        return FsTarget::Tmpfs(alloc::string::String::from(rel));
    }
    // /data paths always go to disk data (FAT32 or ext2 /data fallback),
    // even when ext2 is mounted at root.
    if abs_path.starts_with("/data/") {
        let rel = abs_path.strip_prefix("/data/").unwrap_or("");
        return FsTarget::DiskData(alloc::string::String::from(rel));
    }
    if crate::fs::procfs::path_node(abs_path).is_some()
        || abs_path == "/dev"
        || abs_path.starts_with("/dev/")
        || crate::fs::ramdisk::ramdisk_lookup(abs_path).is_some()
    {
        return FsTarget::Ramdisk;
    }
    // When ext2 is mounted at root, route non-ramdisk paths to ext2.
    if crate::fs::ext2::is_mounted()
        && let Some(rel) = ext2_root_path(abs_path)
    {
        return FsTarget::DiskData(alloc::string::String::from(rel));
    }
    FsTarget::Ramdisk
}

fn create_parent_is_read_only(abs_path: &str) -> bool {
    let parent = parent_path(abs_path);
    parent != "/"
        && (crate::fs::procfs::path_node(parent).is_some()
            || parent == "/dev"
            || parent.starts_with("/dev/")
            || crate::fs::ramdisk::ramdisk_lookup(parent).is_some())
}

/// `chmod(path, mode)` — change file mode bits (syscall 90).
fn sys_linux_chmod(path_ptr: u64, mode_arg: u64) -> u64 {
    let mut buf = [0u8; 512];
    let raw = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };
    let abs = match resolve_existing_fs_path(&resolve_path(&current_cwd(), raw), true) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let mode = (mode_arg & 0o7777) as u16;

    // Only owner or root can chmod.
    let (_, _, euid, _) = current_process_ids();

    match resolve_fs_target(&abs) {
        FsTarget::Tmpfs(rel) => {
            let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
            let stat = match tmpfs.stat(&rel) {
                Ok(s) => s,
                Err(_) => return NEG_ENOENT,
            };
            if euid != 0 && euid != stat.uid {
                return NEG_EPERM;
            }
            if tmpfs.chmod(&rel, mode).is_err() {
                return NEG_ENOENT;
            }
            0
        }
        FsTarget::DiskData(rel) => {
            if euid != 0 {
                let (owner, _, _) = match data_file_metadata(&rel) {
                    Some(m) => m,
                    None => return NEG_ENOENT,
                };
                if euid != owner {
                    return NEG_EPERM;
                }
            }
            data_chmod(&rel, mode)
        }
        FsTarget::Ramdisk => NEG_EROFS,
    }
}

/// `fchmod(fd, mode)` — change file mode bits by fd (syscall 91).
fn sys_linux_fchmod(fd: u64, mode_arg: u64) -> u64 {
    let fd_idx = fd as usize;
    if fd_idx >= MAX_FDS {
        return NEG_EBADF;
    }
    let entry = match current_fd_entry(fd_idx) {
        Some(e) => e,
        None => return NEG_EBADF,
    };
    let mode = (mode_arg & 0o7777) as u16;
    let (_, _, euid, _) = current_process_ids();

    match &entry.backend {
        FdBackend::Tmpfs { path } => {
            let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
            let stat = match tmpfs.stat(path) {
                Ok(s) => s,
                Err(_) => return NEG_ENOENT,
            };
            if euid != 0 && euid != stat.uid {
                return NEG_EPERM;
            }
            if tmpfs.chmod(path, mode).is_err() {
                return NEG_ENOENT;
            }
            0
        }
        FdBackend::Fat32Disk { path, .. } | FdBackend::Ext2Disk { path, .. } => {
            if euid != 0 {
                let (owner, _, _) = match data_file_metadata(path) {
                    Some(m) => m,
                    None => return NEG_ENOENT,
                };
                if euid != owner {
                    return NEG_EPERM;
                }
            }
            data_chmod(path, mode)
        }
        FdBackend::Ramdisk { .. } => NEG_EROFS,
        _ => NEG_EBADF,
    }
}

/// `chown(path, uid, gid)` — change file owner (syscall 92).
/// Only root can change file ownership.
fn sys_linux_chown(path_ptr: u64, uid_arg: u64, gid_arg: u64) -> u64 {
    let mut buf = [0u8; 512];
    let raw = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };
    let abs = match resolve_existing_fs_path(&resolve_path(&current_cwd(), raw), true) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let new_uid = uid_arg as u32;
    let new_gid = gid_arg as u32;

    let (_, _, euid, _) = current_process_ids();
    if euid != 0 {
        return NEG_EPERM;
    }

    match resolve_fs_target(&abs) {
        FsTarget::Tmpfs(rel) => {
            let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
            if tmpfs.chown(&rel, new_uid, new_gid).is_err() {
                return NEG_ENOENT;
            }
            0
        }
        FsTarget::DiskData(rel) => data_chown(&rel, new_uid, new_gid),
        FsTarget::Ramdisk => NEG_EROFS,
    }
}

/// `fchown(fd, uid, gid)` — change file owner by fd (syscall 93).
fn sys_linux_fchown(fd: u64, uid_arg: u64, gid_arg: u64) -> u64 {
    let fd_idx = fd as usize;
    if fd_idx >= MAX_FDS {
        return NEG_EBADF;
    }
    let entry = match current_fd_entry(fd_idx) {
        Some(e) => e,
        None => return NEG_EBADF,
    };
    let new_uid = uid_arg as u32;
    let new_gid = gid_arg as u32;

    let (_, _, euid, _) = current_process_ids();
    if euid != 0 {
        return NEG_EPERM;
    }

    match &entry.backend {
        FdBackend::Tmpfs { path } => {
            let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
            if tmpfs.chown(path, new_uid, new_gid).is_err() {
                return NEG_ENOENT;
            }
            0
        }
        FdBackend::Fat32Disk { path, .. } | FdBackend::Ext2Disk { path, .. } => {
            data_chown(path, new_uid, new_gid)
        }
        FdBackend::Ramdisk { .. } => NEG_EROFS,
        _ => NEG_EBADF,
    }
}

// ---------------------------------------------------------------------------
// T017: lseek(fd, offset, whence)
// ---------------------------------------------------------------------------

fn sys_linux_lseek(fd: u64, offset: u64, whence: u64) -> u64 {
    let fd = fd as usize;
    if !(3..MAX_FDS).contains(&fd) {
        return NEG_EBADF;
    }

    let entry = match current_fd_entry(fd) {
        Some(e) => e,
        None => return NEG_EBADF,
    };

    const SEEK_SET: u64 = 0;
    const SEEK_CUR: u64 = 1;
    const SEEK_END: u64 = 2;

    let file_len = match &entry.backend {
        FdBackend::Stdout
        | FdBackend::Stdin
        | FdBackend::PipeRead { .. }
        | FdBackend::PipeWrite { .. }
        | FdBackend::Dir { .. }
        | FdBackend::DevNull
        | FdBackend::DevZero
        | FdBackend::DevUrandom
        | FdBackend::DevFull
        | FdBackend::DeviceTTY { .. }
        | FdBackend::PtyMaster { .. }
        | FdBackend::PtySlave { .. }
        | FdBackend::Proc { .. }
        | FdBackend::Socket { .. }
        | FdBackend::UnixSocket { .. }
        | FdBackend::Epoll { .. } => return NEG_EINVAL, // not seekable
        FdBackend::Ramdisk { content_len, .. } => *content_len,
        FdBackend::Tmpfs { path } => {
            let tmpfs = crate::fs::tmpfs::TMPFS.lock();
            match tmpfs.file_size(path) {
                Ok(len) => len,
                Err(_) => return NEG_ENOENT,
            }
        }
        FdBackend::Fat32Disk { file_size, .. } | FdBackend::Ext2Disk { file_size, .. } => {
            *file_size as usize
        }
    };

    let offset = offset as i64;

    let new_offset: i64 = match whence {
        SEEK_SET => offset,
        SEEK_CUR => match (entry.offset as i64).checked_add(offset) {
            Some(v) => v,
            None => return NEG_EINVAL,
        },
        SEEK_END => match (file_len as i64).checked_add(offset) {
            Some(v) => v,
            None => return NEG_EINVAL,
        },
        _ => return NEG_EINVAL,
    };

    if new_offset < 0 || new_offset as usize > file_len {
        return NEG_EINVAL;
    }

    // Update offset in per-process FD table.
    with_current_fd_mut(fd, |slot| {
        if let Some(e) = slot {
            e.offset = new_offset as usize;
        }
    });
    new_offset as u64
}

// ---------------------------------------------------------------------------
// T018: mmap(addr, len, prot, flags[from SYSCALL_ARG3], fd, offset)
//
// Only MAP_PRIVATE|MAP_ANONYMOUS (flags 0x22) with fd=-1 is supported.
// ---------------------------------------------------------------------------

fn sys_linux_mmap(addr_hint: u64, len: u64, prot: u64) -> u64 {
    // Read flags from SYSCALL_ARG3 (r10 at syscall entry).
    // SAFETY: single-CPU, read after every SYSCALL entry stores to SYSCALL_ARG3.
    let flags = per_core_syscall_arg3();

    const MAP_PRIVATE: u64 = 0x02;
    const MAP_ANONYMOUS: u64 = 0x20;
    if flags & MAP_ANONYMOUS == 0 {
        log::warn!(
            "[mmap] non-anonymous mmap not supported (flags={:#x})",
            flags
        );
        return NEG_EINVAL;
    }
    // Mask prot and flags to supported bits only.
    const PROT_MASK: u64 = 0x7; // PROT_READ | PROT_WRITE | PROT_EXEC
    let prot = prot & PROT_MASK;
    let flags = flags & (MAP_PRIVATE | MAP_ANONYMOUS);

    let len = if len == 0 {
        return NEG_EINVAL;
    } else {
        len
    };
    let pages = len.div_ceil(4096);

    let pid = crate::process::current_pid();

    // Determine base address: use process mmap_next or default ANON_MMAP_BASE.
    let base = {
        let mut table = crate::process::PROCESS_TABLE.lock();
        let proc = match table.find_mut(pid) {
            Some(p) => p,
            None => return NEG_EINVAL,
        };
        if proc.mmap_next == 0 {
            proc.mmap_next = ANON_MMAP_BASE;
        }
        // Hint address is ignored: always allocate linearly.
        let _ = addr_hint;
        let base = proc.mmap_next;
        let total_size = match pages.checked_mul(4096) {
            Some(s) => s,
            None => return NEG_EINVAL,
        };
        proc.mmap_next = match base.checked_add(total_size) {
            Some(v) => v,
            None => return NEG_EINVAL,
        };
        base
    };

    // Validate that the entire range fits in canonical user space (< 0x0000_8000_0000_0000).
    let total_size = match pages.checked_mul(4096) {
        Some(s) => s,
        None => return NEG_EINVAL,
    };
    let range_end = match base.checked_add(total_size) {
        Some(e) => e,
        None => return NEG_EINVAL,
    };
    if range_end > 0x0000_8000_0000_0000 {
        return NEG_EINVAL;
    }

    // Phase 36: demand paging — do NOT allocate physical frames here.
    // Pages are mapped lazily by the page fault handler on first access.
    // Only record the VMA with protection and flags metadata.

    // Record the mapping in the process's tracking list.
    {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(proc) = table.find_mut(pid) {
            proc.mappings.push(crate::process::MemoryMapping {
                start: base,
                len: total_size,
                prot,
                flags,
            });
        }
    }

    base
}

// ---------------------------------------------------------------------------
// T019: munmap(addr, len) — Phase 33: reclaim frames + TLB shootdown
// ---------------------------------------------------------------------------

fn sys_linux_munmap(addr: u64, len: u64) -> u64 {
    // Validate: page-aligned address and non-zero length.
    if addr & 0xFFF != 0 || len == 0 {
        return NEG_EINVAL;
    }

    // Must be in userspace canonical range.
    if addr >= 0x0000_8000_0000_0000 {
        return NEG_EINVAL;
    }

    let pages = len.div_ceil(4096) as usize;

    // Validate range doesn't overflow.
    let total_size = match (pages as u64).checked_mul(4096) {
        Some(s) => s,
        None => return NEG_EINVAL,
    };
    if addr.checked_add(total_size).is_none() {
        return NEG_EINVAL;
    }

    use x86_64::structures::paging::{Mapper, Page, Size4KiB};

    // SAFETY: current CR3 is the calling process's page table; this is the
    // same approach used by sys_linux_mmap.
    let mut mapper = unsafe { crate::mm::paging::get_mapper() };

    let mut unmapped_addrs: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
    for i in 0..pages {
        let page_addr = addr + (i as u64 * 4096);
        let page: Page<Size4KiB> = Page::containing_address(x86_64::VirtAddr::new(page_addr));

        // Try to unmap — silently skip pages that aren't mapped (POSIX allows this).
        match mapper.unmap(page) {
            Ok((frame, flush)) => {
                // Skip the local TLB flush here — we batch a single shootdown
                // (which includes a local invlpg) after the loop.
                flush.ignore();
                crate::mm::frame_allocator::free_frame(frame.start_address().as_u64());
                unmapped_addrs.push(page_addr);
            }
            Err(_) => {
                // Page wasn't mapped — skip silently.
            }
        }
    }
    let _freed_count = unmapped_addrs.len();

    // SMP TLB shootdown: invalidate only pages that were actually unmapped.
    for &page_addr in &unmapped_addrs {
        crate::smp::tlb::tlb_shootdown(page_addr);
    }

    // Update mapping tracking list: handle full removal, shrink, and split.
    let pid = crate::process::current_pid();
    {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(proc) = table.find_mut(pid) {
            let unmap_start = addr;
            let unmap_end = addr + total_size;
            let mut new_mappings = alloc::vec::Vec::new();
            proc.mappings.retain_mut(|m| {
                let m_end = m.start + m.len;
                if m.start >= unmap_end || m_end <= unmap_start {
                    // No overlap — keep as-is.
                    return true;
                }
                if m.start >= unmap_start && m_end <= unmap_end {
                    // Fully contained — remove.
                    return false;
                }
                if m.start < unmap_start && m_end > unmap_end {
                    // Hole punch: split into two mappings.
                    // Keep the head portion in place.
                    let tail = crate::process::MemoryMapping {
                        start: unmap_end,
                        len: m_end - unmap_end,
                        prot: m.prot,
                        flags: m.flags,
                    };
                    new_mappings.push(tail);
                    m.len = unmap_start - m.start;
                    return true;
                }
                if m.start < unmap_start {
                    // Overlap at tail — shrink.
                    m.len = unmap_start - m.start;
                } else {
                    // Overlap at head — shrink.
                    let new_start = unmap_end;
                    m.len = m_end - new_start;
                    m.start = new_start;
                }
                true
            });
            proc.mappings.extend(new_mappings);
        }
    }

    0
}

// ---------------------------------------------------------------------------
// Phase 36: mprotect(addr, len, prot) — change page permissions on mapped
// regions. Updates PTEs in-place and splits VMAs at mprotect boundaries.
// ---------------------------------------------------------------------------

fn sys_mprotect(addr: u64, len: u64, prot: u64) -> u64 {
    // Mask prot to supported POSIX bits only.
    let prot = prot & 0x7; // PROT_READ | PROT_WRITE | PROT_EXEC

    // Validate: page-aligned address and non-zero length.
    if addr & 0xFFF != 0 || len == 0 {
        return NEG_EINVAL;
    }
    if addr >= 0x0000_8000_0000_0000 {
        return NEG_EINVAL;
    }

    let total_size = len.div_ceil(4096) * 4096; // round up to page boundary
    let mprotect_end = match addr.checked_add(total_size) {
        Some(e) => e,
        None => return NEG_EINVAL,
    };

    const PROT_READ: u64 = 0x1;
    const PROT_WRITE: u64 = 0x2;
    const PROT_EXEC: u64 = 0x4;

    // Validate that the entire range is covered by VMAs (or stack/brk regions).
    // For now, we are permissive: if the address falls outside tracked VMAs
    // (e.g. stack, brk, ELF segments), we still update PTEs but don't fail.
    // This matches the musl stack guard use case where mprotect targets
    // ELF loader-mapped regions not tracked as VMAs.

    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::{PageTable, PageTableFlags};

    let phys_off = crate::mm::phys_offset();
    let phys_offset_va = x86_64::VirtAddr::new(phys_off);

    let (cr3_frame, _) = Cr3::read();
    let pml4_phys = cr3_frame.start_address().as_u64();

    // Build the new PTE flags from prot.
    let mut new_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if prot & PROT_WRITE != 0 {
        new_flags |= PageTableFlags::WRITABLE;
    }
    if prot & PROT_EXEC == 0 {
        new_flags |= PageTableFlags::NO_EXECUTE;
    }
    // PROT_NONE: clear PRESENT to trap all accesses.
    let is_prot_none = prot & (PROT_READ | PROT_WRITE | PROT_EXEC) == 0;

    let pages = total_size / 4096;
    let mut changed_addrs: alloc::vec::Vec<u64> = alloc::vec::Vec::new();

    for i in 0..pages {
        let page_addr = addr + i * 4096;
        let p4_idx = ((page_addr >> 39) & 0x1FF) as usize;
        let p3_idx = ((page_addr >> 30) & 0x1FF) as usize;
        let p2_idx = ((page_addr >> 21) & 0x1FF) as usize;
        let p1_idx = ((page_addr >> 12) & 0x1FF) as usize;

        unsafe {
            let pml4: &mut PageTable = &mut *(phys_offset_va + pml4_phys).as_mut_ptr::<PageTable>();
            if !pml4[p4_idx].flags().contains(PageTableFlags::PRESENT) {
                continue; // Not yet demand-mapped — VMA prot update suffices.
            }
            let pdpt: &mut PageTable =
                &mut *(phys_offset_va + pml4[p4_idx].addr().as_u64()).as_mut_ptr::<PageTable>();
            if !pdpt[p3_idx].flags().contains(PageTableFlags::PRESENT) {
                continue;
            }
            let pd: &mut PageTable =
                &mut *(phys_offset_va + pdpt[p3_idx].addr().as_u64()).as_mut_ptr::<PageTable>();
            if !pd[p2_idx].flags().contains(PageTableFlags::PRESENT) {
                continue;
            }
            let pt: &mut PageTable =
                &mut *(phys_offset_va + pd[p2_idx].addr().as_u64()).as_mut_ptr::<PageTable>();
            if !pt[p1_idx].flags().contains(PageTableFlags::PRESENT) && !is_prot_none {
                continue; // Not yet demand-mapped.
            }

            if pt[p1_idx].flags().contains(PageTableFlags::PRESENT) {
                let old_flags = pt[p1_idx].flags();
                let old_addr = pt[p1_idx].addr();
                let is_cow = old_flags.contains(PageTableFlags::BIT_9);
                let final_flags = if is_prot_none {
                    // Clear PRESENT to make the page trap on any access.
                    (old_flags & !PageTableFlags::PRESENT & !PageTableFlags::WRITABLE)
                        | PageTableFlags::BIT_10 // mark as guard page
                } else {
                    // Preserve CoW marker. If the page is CoW, keep it
                    // non-writable — the CoW fault handler will make it
                    // writable after copying.
                    let mut f = new_flags;
                    if is_cow {
                        f |= PageTableFlags::BIT_9;
                        f &= !PageTableFlags::WRITABLE;
                    }
                    f
                };
                pt[p1_idx].set_addr(old_addr, final_flags);
                changed_addrs.push(page_addr);
            }
        }
    }

    // TLB shootdown for all changed pages.
    for &page_addr in &changed_addrs {
        crate::smp::tlb::tlb_shootdown(page_addr);
    }

    // Update VMA protection bits and split VMAs at mprotect boundaries.
    let pid = crate::process::current_pid();
    {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(proc) = table.find_mut(pid) {
            let mut new_mappings = alloc::vec::Vec::new();
            for m in proc.mappings.iter_mut() {
                let m_end = m.start + m.len;
                // Skip non-overlapping VMAs.
                if m.start >= mprotect_end || m_end <= addr {
                    continue;
                }
                // Fully contained — just update prot.
                if m.start >= addr && m_end <= mprotect_end {
                    m.prot = prot;
                    continue;
                }
                // Partial overlap — need to split.
                if m.start < addr && m_end > mprotect_end {
                    // Middle split: 3 VMAs (head, middle, tail).
                    let tail = crate::process::MemoryMapping {
                        start: mprotect_end,
                        len: m_end - mprotect_end,
                        prot: m.prot,
                        flags: m.flags,
                    };
                    let middle = crate::process::MemoryMapping {
                        start: addr,
                        len: mprotect_end - addr,
                        prot,
                        flags: m.flags,
                    };
                    new_mappings.push(middle);
                    new_mappings.push(tail);
                    m.len = addr - m.start; // head
                } else if m.start < addr {
                    // Overlap at tail of VMA — split into head + modified tail.
                    let modified = crate::process::MemoryMapping {
                        start: addr,
                        len: m_end - addr,
                        prot,
                        flags: m.flags,
                    };
                    new_mappings.push(modified);
                    m.len = addr - m.start;
                } else {
                    // Overlap at head of VMA — split into modified head + tail.
                    let tail = crate::process::MemoryMapping {
                        start: mprotect_end,
                        len: m_end - mprotect_end,
                        prot: m.prot,
                        flags: m.flags,
                    };
                    new_mappings.push(tail);
                    m.len = mprotect_end - m.start;
                    m.prot = prot;
                }
            }
            proc.mappings.extend(new_mappings);
        }
    }

    0
}

// ---------------------------------------------------------------------------
// Phase 33 Track F: meminfo syscall (0x1001)
//
// Writes a text summary of kernel memory statistics into a user buffer.
// arg0 = user buffer address, arg1 = buffer length.
// Returns number of bytes written, or 0 on error.
// ---------------------------------------------------------------------------

fn sys_meminfo(buf_addr: u64, buf_len: u64) -> u64 {
    use core::fmt::Write;

    if buf_addr == 0 || buf_len == 0 {
        return 0;
    }

    // Gather stats
    let heap = crate::mm::heap::heap_stats();
    let frames = crate::mm::frame_allocator::frame_stats();
    let slabs = crate::mm::slab::all_slab_stats();

    // Format into a stack buffer
    let mut tmp = [0u8; 2048];
    let mut writer = BufWriter::new(&mut tmp);

    let _ = writeln!(writer, "=== Kernel Memory Info ===");
    let _ = writeln!(writer);
    let _ = writeln!(writer, "Heap:");
    let _ = writeln!(
        writer,
        "  total: {} KiB  used: {} KiB  free: {} KiB",
        heap.total_size / 1024,
        heap.used_bytes / 1024,
        heap.free_bytes / 1024
    );
    let _ = writeln!(
        writer,
        "  allocs: {}  deallocs: {}",
        heap.alloc_count, heap.dealloc_count
    );
    let _ = writeln!(writer);
    let _ = writeln!(writer, "Frames (4 KiB pages):");
    let _ = writeln!(
        writer,
        "  total: {}  free: {}  allocated: {}",
        frames.total_frames, frames.free_frames, frames.allocated_frames
    );
    let _ = writeln!(
        writer,
        "  memory: {} MiB total, {} MiB free",
        frames.total_frames * 4 / 1024,
        frames.free_frames * 4 / 1024
    );
    let _ = write!(writer, "  buddy orders:");
    for (order, &count) in frames.free_by_order.iter().enumerate() {
        if count > 0 {
            let _ = write!(writer, " o{}={}", order, count);
        }
    }
    let _ = writeln!(writer);
    let _ = writeln!(writer);
    let _ = writeln!(writer, "Slab Caches:");
    fn fmt_slab(w: &mut BufWriter<'_>, name: &str, s: &kernel_core::slab::SlabStats) {
        let _ = writeln!(
            w,
            "  {}: slabs={} active={} free={}",
            name, s.total_slabs, s.active_objects, s.free_slots
        );
    }
    fmt_slab(&mut writer, "task(512B) ", &slabs.task);
    fmt_slab(&mut writer, "fd(64B)   ", &slabs.fd);
    fmt_slab(&mut writer, "endpt(128B)", &slabs.endpoint);
    fmt_slab(&mut writer, "pipe(4KiB)", &slabs.pipe);
    fmt_slab(&mut writer, "sock(256B)", &slabs.socket);

    let written = writer.pos;

    // Copy to user buffer
    let copy_len = written.min(buf_len as usize);
    if crate::mm::user_mem::copy_to_user(buf_addr, &tmp[..copy_len]).is_err() {
        return 0;
    }

    copy_len as u64
}

/// Tiny stack buffer writer for formatting meminfo output.
struct BufWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> BufWriter<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }
}

impl core::fmt::Write for BufWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let remaining = self.buf.len() - self.pos;
        let len = bytes.len().min(remaining);
        self.buf[self.pos..self.pos + len].copy_from_slice(&bytes[..len]);
        self.pos += len;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Phase 47 Track A: framebuffer info syscall (0x1002)
//
// Writes a packed FbInfo struct into a user-supplied buffer.
// arg0 = user buffer pointer, arg1 = buffer length (must be >= 20 bytes)
// Returns 0 on success, NEG_EINVAL on error.
// ---------------------------------------------------------------------------

#[repr(C)]
struct FbInfo {
    width: u32,
    height: u32,
    stride: u32,
    bpp: u32,
    pixel_format: u32,
}

fn sys_framebuffer_info(buf_addr: u64, buf_len: u64) -> u64 {
    const USER_LIMIT: u64 = 0x0000_8000_0000_0000;
    const FB_INFO_SIZE: u64 = core::mem::size_of::<FbInfo>() as u64;

    if buf_addr == 0 || buf_addr >= USER_LIMIT || buf_len < FB_INFO_SIZE {
        return NEG_EINVAL;
    }

    let (width, height, stride, bpp, pixel_format) = match crate::fb::framebuffer_raw_info() {
        Some(info) => info,
        None => return NEG_EINVAL,
    };

    let pixel_format_val: u32 = match pixel_format {
        bootloader_api::info::PixelFormat::Rgb => 0,
        bootloader_api::info::PixelFormat::Bgr => 1,
        _ => 2,
    };

    let info = FbInfo {
        width: width as u32,
        height: height as u32,
        stride: stride as u32,
        bpp: bpp as u32,
        pixel_format: pixel_format_val,
    };

    let info_bytes = unsafe {
        core::slice::from_raw_parts(&info as *const FbInfo as *const u8, FB_INFO_SIZE as usize)
    };
    if crate::mm::user_mem::copy_to_user(buf_addr, info_bytes).is_err() {
        return NEG_EFAULT;
    }
    0
}

// ---------------------------------------------------------------------------
// Phase 47 Track A: framebuffer mmap syscall (0x1003)
//
// Maps the physical framebuffer into the calling process's address space.
// Returns the userspace virtual address, or NEG_EINVAL on error.
// ---------------------------------------------------------------------------

fn sys_framebuffer_mmap() -> u64 {
    let (buf_virt, byte_len) = match crate::fb::framebuffer_buf_addr() {
        Some(v) => v,
        None => return NEG_EINVAL,
    };

    let phys_off = crate::mm::phys_offset();
    // checked_sub guards against buf_virt < phys_off (would underflow to a
    // spurious physical address and map arbitrary memory into userspace).
    let buf_phys = match buf_virt.checked_sub(phys_off) {
        Some(v) if v % 4096 == 0 => v,
        _ => return NEG_EINVAL,
    };

    let num_pages = (byte_len as u64).div_ceil(4096);
    let total_size = num_pages * 4096;

    // Build array of PhysFrame for each page of the framebuffer.
    let mut frames = alloc::vec::Vec::new();
    for i in 0..num_pages {
        let phys_addr = x86_64::PhysAddr::new(buf_phys + i * 4096);
        let frame = match x86_64::structures::paging::PhysFrame::<
            x86_64::structures::paging::Size4KiB,
        >::from_start_address(phys_addr)
        {
            Ok(f) => f,
            Err(_) => return NEG_EINVAL,
        };
        frames.push(frame);
    }

    let pid = crate::process::current_pid();

    // Atomically claim the framebuffer via compare-and-swap before touching
    // page tables.  This eliminates the TOCTOU window that the old two-step
    // check-then-store had: two racing processes can no longer both observe
    // owner==0 and proceed to map.
    if !crate::fb::try_yield_console(pid) {
        return NEG_EBUSY;
    }

    // Determine the virtual address for the mapping in the process address space.
    // Release the console claim on any early error so another process can retry.
    let virt_addr = {
        let mut table = crate::process::PROCESS_TABLE.lock();
        let proc = match table.find_mut(pid) {
            Some(p) => p,
            None => {
                crate::fb::release_console_claim(pid);
                return NEG_EINVAL;
            }
        };
        if proc.mmap_next == 0 {
            proc.mmap_next = ANON_MMAP_BASE;
        }
        // Round up to page boundary.
        let base = (proc.mmap_next + 4095) & !4095;
        // Guard against pushing mmap_next past the canonical user-space limit.
        const USER_SPACE_END: u64 = 0x0000_8000_0000_0000;
        let end = match base.checked_add(total_size) {
            Some(v) if v <= USER_SPACE_END => v,
            _ => {
                crate::fb::release_console_claim(pid);
                return NEG_EINVAL;
            }
        };
        proc.mmap_next = end;
        base
    };

    // Get the process page table and map the frames.
    let cr3_phys = {
        let table = crate::process::PROCESS_TABLE.lock();
        match table.find(pid).and_then(|p| p.page_table_root) {
            Some(phys) => phys,
            None => {
                crate::fb::release_console_claim(pid);
                return NEG_EINVAL;
            }
        }
    };

    let cr3_frame = match x86_64::structures::paging::PhysFrame::<
        x86_64::structures::paging::Size4KiB,
    >::from_start_address(cr3_phys)
    {
        Ok(f) => f,
        Err(_) => {
            crate::fb::release_console_claim(pid);
            return NEG_EINVAL;
        }
    };

    let mut mapper = unsafe { crate::mm::mapper_for_frame(cr3_frame) };

    use x86_64::structures::paging::PageTableFlags;
    // BIT_11 is an OS-available bit used here to mark "device/hardware frame —
    // do not return to the frame allocator on process teardown".  The frame
    // allocator only owns system-RAM frames; the UEFI framebuffer is MMIO.
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE
        | PageTableFlags::BIT_11;

    if unsafe { crate::mm::user_space::map_user_frames(&mut mapper, virt_addr, &frames, flags) }
        .is_err()
    {
        crate::fb::release_console_claim(pid);
        return NEG_EINVAL;
    }

    // Record the mapping in the process table.
    {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(proc) = table.find_mut(pid) {
            proc.mappings.push(crate::process::MemoryMapping {
                start: virt_addr,
                len: total_size,
                prot: 3,  // PROT_READ | PROT_WRITE
                flags: 1, // MAP_SHARED
            });
        }
    }

    // Ownership was claimed atomically at the top of this function via
    // try_yield_console (compare_exchange).  No second store needed here.

    log::info!(
        "[framebuffer_mmap] pid={} mapped {} pages @ {:#x}",
        pid,
        num_pages,
        virt_addr
    );
    virt_addr
}

// ---------------------------------------------------------------------------
// Phase 47 Track B: raw scancode syscall (0x1004)
//
// Returns the next raw PS/2 scancode from the keyboard ring buffer,
// or 0 if no scancode is available (non-blocking).
// ---------------------------------------------------------------------------

fn sys_read_scancode() -> u64 {
    match super::interrupts::read_scancode() {
        Some(sc) => sc as u64,
        None => 0,
    }
}

// ---------------------------------------------------------------------------
// T020: brk(addr)
// ---------------------------------------------------------------------------

fn sys_linux_brk(addr: u64) -> u64 {
    let pid = crate::process::current_pid();

    // Always initialise brk_current to BRK_BASE if it is still 0, regardless
    // of the requested addr.  This ensures that even a first call with a
    // nonzero addr has a valid base to grow from, and if page mapping fails
    // later we still have a consistent brk_current.
    let current = {
        let mut table = crate::process::PROCESS_TABLE.lock();
        match table.find_mut(pid) {
            Some(p) => {
                if p.brk_current == 0 {
                    p.brk_current = BRK_BASE;
                }
                p.brk_current
            }
            None => return 0,
        }
    };

    // brk(0) or no-advance: just return current break.
    if addr == 0 || addr <= current {
        return current;
    }

    // Align new break up to page boundary.
    let new_brk = match addr.checked_add(0xFFF) {
        Some(v) => v & !0xFFF,
        None => return current,
    };
    // Reject non-canonical / kernel-range addresses.
    if new_brk > 0x0000_7FFF_FFFF_FFFF {
        return current;
    }
    let pages_needed = (new_brk - current) / 4096;

    use x86_64::{
        VirtAddr,
        structures::paging::{Mapper, Page, PageTableFlags, Size4KiB},
    };
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;
    // SAFETY: current CR3 is the process's page table.
    let mut mapper = unsafe { crate::mm::paging::get_mapper() };
    let mut frame_alloc = crate::mm::paging::GlobalFrameAlloc;
    let phys_off = crate::mm::phys_offset();

    for i in 0..pages_needed {
        let vaddr = VirtAddr::new(current + i * 4096);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);
        let frame = match crate::mm::frame_allocator::allocate_frame() {
            Some(f) => f,
            None => {
                log::warn!("[brk] out of frames at page {}/{}", i, pages_needed);
                return current; // return old brk to signal failure
            }
        };
        unsafe {
            let ptr = (phys_off + frame.start_address().as_u64()) as *mut u8;
            core::ptr::write_bytes(ptr, 0, 4096);
        }
        // SAFETY: mapper covers current CR3; frame was just allocated; page is unmapped.
        match unsafe { mapper.map_to(page, frame, flags, &mut frame_alloc) } {
            Ok(flush) => flush.flush(),
            Err(_) => {
                log::warn!("[brk] map_to failed at page {}", i);
                return current;
            }
        }
    }

    {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(p) = table.find_mut(pid) {
            p.brk_current = new_brk;
        }
    }

    // Omit per-call log to avoid flooding serial during high-alloc workloads.
    new_brk
}

// ---------------------------------------------------------------------------
// T023: writev(fd, iov, iovcnt)
// ---------------------------------------------------------------------------

fn sys_linux_writev(fd: u64, iov_ptr: u64, iovcnt: u64) -> u64 {
    if iovcnt > 1024 {
        return NEG_EINVAL;
    }
    let iovcnt = iovcnt as usize;
    let mut total = 0u64;
    for i in 0..iovcnt {
        // struct iovec { void *base (8B), size_t len (8B) }
        let offset = match (i as u64).checked_mul(16) {
            Some(v) => v,
            None => return NEG_EFAULT,
        };
        let iov_addr = match iov_ptr.checked_add(offset) {
            Some(a) => a,
            None => return NEG_EFAULT,
        };
        let mut iov_bytes = [0u8; 16];
        if crate::mm::user_mem::copy_from_user(&mut iov_bytes, iov_addr).is_err() {
            if total == 0 {
                return NEG_EFAULT;
            }
            break;
        }
        let base = u64::from_ne_bytes(iov_bytes[0..8].try_into().unwrap());
        let len = u64::from_ne_bytes(iov_bytes[8..16].try_into().unwrap());
        if len == 0 {
            continue;
        }
        let written = sys_linux_write(fd, base, len);
        if (written as i64) < 0 {
            // If no bytes transferred yet, propagate the error.
            if total == 0 {
                return written;
            }
            break;
        }
        if written == 0 {
            break;
        }
        total += written;
        // Short write: fewer bytes than requested means we should stop.
        if written < len {
            break;
        }
    }
    total
}

// ---------------------------------------------------------------------------
// T023: readv(fd, iov, iovcnt)
// ---------------------------------------------------------------------------

fn sys_linux_readv(fd: u64, iov_ptr: u64, iovcnt: u64) -> u64 {
    if iovcnt > 1024 {
        return NEG_EINVAL;
    }
    let iovcnt = iovcnt as usize;
    let mut total = 0u64;
    for i in 0..iovcnt {
        let offset = match (i as u64).checked_mul(16) {
            Some(v) => v,
            None => return NEG_EFAULT,
        };
        let iov_addr = match iov_ptr.checked_add(offset) {
            Some(a) => a,
            None => return NEG_EFAULT,
        };
        let mut iov_bytes = [0u8; 16];
        if crate::mm::user_mem::copy_from_user(&mut iov_bytes, iov_addr).is_err() {
            if total == 0 {
                return NEG_EFAULT;
            }
            break;
        }
        let base = u64::from_ne_bytes(iov_bytes[0..8].try_into().unwrap());
        let len = u64::from_ne_bytes(iov_bytes[8..16].try_into().unwrap());
        if len == 0 {
            continue;
        }
        let n = sys_linux_read(fd, base, len);
        if (n as i64) < 0 {
            // If no bytes transferred yet, propagate the error.
            if total == 0 {
                return n;
            }
            break;
        }
        if n == 0 {
            break; // EOF
        }
        total += n;
        // Short read: fewer bytes than requested means EOF / no more data.
        if n < len {
            break;
        }
    }
    total
}

// ---------------------------------------------------------------------------
// T024: getcwd(buf, size) — return per-process working directory
// ---------------------------------------------------------------------------

fn sys_linux_getcwd(buf_ptr: u64, size: u64) -> u64 {
    let cwd = current_cwd();
    let cwd_bytes = cwd.as_bytes();
    let total_len = cwd_bytes.len() + 1; // include null terminator
    if (size as usize) < total_len {
        const NEG_ERANGE: u64 = (-34_i64) as u64;
        return NEG_ERANGE;
    }
    // Copy path, then write a single null terminator — no heap allocation.
    if crate::mm::user_mem::copy_to_user(buf_ptr, cwd_bytes).is_err() {
        return NEG_EFAULT;
    }
    let terminator_ptr = match buf_ptr.checked_add(cwd_bytes.len() as u64) {
        Some(p) => p,
        None => return NEG_EFAULT,
    };
    if crate::mm::user_mem::copy_to_user(terminator_ptr, &[0u8]).is_err() {
        return NEG_EFAULT;
    }
    // Linux getcwd returns the length of the path (including null terminator).
    total_len as u64
}

// ---------------------------------------------------------------------------
// T024: chdir(path) — resolve path, validate directory, update process cwd
// ---------------------------------------------------------------------------

fn sys_linux_chdir(path_ptr: u64) -> u64 {
    let _mount_guard = MOUNT_OP_LOCK.lock();
    let mut buf = [0u8; 512];
    let name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    let lexical = match resolve_path_from_dirfd(AT_FDCWD, name) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let resolved = match resolve_existing_fs_path(&lexical, true) {
        Ok(path) => path,
        Err(err) => return err,
    };

    // Phase 27: Execute (search) permission on target directory.
    if let Some((fu, fg, fm)) = path_metadata(&resolved) {
        let (_, _, euid, egid) = current_process_ids();
        if !check_permission(fu, fg, fm, euid, egid, 1) {
            return NEG_EACCES;
        }
    }

    // Verify the resolved path exists and is a directory.
    if !is_directory(&resolved) {
        // Path is not a directory — check if it exists at all to choose error.
        if let Some(rel) = tmpfs_relative_path(&resolved) {
            if !rel.is_empty() {
                let tmpfs = crate::fs::tmpfs::TMPFS.lock();
                if tmpfs.stat(rel).is_ok() {
                    return NEG_ENOTDIR;
                }
            }
        } else if crate::fs::ramdisk::ramdisk_lookup(&resolved).is_some() {
            return NEG_ENOTDIR;
        }
        return NEG_ENOENT;
    }

    let pid = crate::process::current_pid();
    let mut table = crate::process::PROCESS_TABLE.lock();
    if let Some(proc) = table.find_mut(pid) {
        proc.cwd = resolved;
    }
    0
}

// ---------------------------------------------------------------------------
// T025: ioctl — TIOCGWINSZ only
// ---------------------------------------------------------------------------

fn sys_linux_ioctl(fd: u64, req: u64, arg: u64) -> u64 {
    // Musl declares ioctl(int, int, ...) — the request code is sign-extended
    // from 32 bits.  Truncate to u32 so _IOR/_IOW constants with bit 31 set
    // (e.g., TIOCGPTN = 0x80045430) compare correctly.
    let req = (req as u32) as u64;
    use kernel_core::tty::{TERMIOS_SIZE, WINSIZE_SIZE};
    const TCGETS: u64 = 0x5401;
    const TCSETS: u64 = 0x5402;
    const TCSETSW: u64 = 0x5403;
    const TCSETSF: u64 = 0x5404;
    const TIOCGPGRP: u64 = 0x540F;
    const TIOCSPGRP: u64 = 0x5410;
    const TIOCGWINSZ: u64 = 0x5413;
    const TIOCSWINSZ: u64 = 0x5414;
    const NEG_ENOTTY: u64 = (-25_i64) as u64;

    const TIOCGPTN: u64 = 0x80045430; // _IOR('T', 0x30, unsigned int)

    // Check if the fd is a TTY or PTY; non-TTY fds return ENOTTY.
    let fd_idx = fd as usize;
    let backend = if fd_idx < MAX_FDS {
        current_fd_entry(fd_idx).map(|e| e.backend.clone())
    } else {
        None
    };
    let is_tty = matches!(
        &backend,
        Some(FdBackend::DeviceTTY { .. })
            | Some(FdBackend::PtyMaster { .. })
            | Some(FdBackend::PtySlave { .. })
    );

    if !is_tty {
        return NEG_ENOTTY;
    }

    // Helper: extract PTY ID from the backend (if it's a PTY FD).
    let pty_id = match &backend {
        Some(FdBackend::PtyMaster { pty_id }) | Some(FdBackend::PtySlave { pty_id }) => {
            Some(*pty_id)
        }
        _ => None,
    };
    let is_pty_master = matches!(&backend, Some(FdBackend::PtyMaster { .. }));

    // TIOCGPTN: return PTY number for master fds.
    if req == TIOCGPTN {
        if let Some(FdBackend::PtyMaster { pty_id }) = &backend {
            let bytes = (*pty_id).to_ne_bytes();
            if crate::mm::user_mem::copy_to_user(arg, &bytes).is_err() {
                return NEG_EFAULT;
            }
            return 0;
        }
        return NEG_EINVAL;
    }

    const TIOCSPTLCK: u64 = 0x40045431;
    const TIOCGRANTPT: u64 = 0x5417;
    const TIOCSCTTY: u64 = 0x540E;
    const TIOCNOTTY: u64 = 0x5422;

    // TIOCSPTLCK: lock/unlock the PTY slave.
    if req == TIOCSPTLCK {
        if let Some(id) = pty_id
            && is_pty_master
        {
            let mut lock_val = [0u8; 4];
            if crate::mm::user_mem::copy_from_user(&mut lock_val, arg).is_err() {
                return NEG_EFAULT;
            }
            let val = i32::from_ne_bytes(lock_val);
            let mut table = crate::pty::PTY_TABLE.lock();
            if let Some(Some(pair)) = table.get_mut(id as usize) {
                pair.locked = val != 0;
                return 0;
            }
            return NEG_EIO;
        }
        return NEG_EINVAL; // not a PTY master
    }

    // TIOCGRANTPT: no-op (permissions not enforced yet).
    if req == TIOCGRANTPT {
        return 0;
    }

    // TIOCSCTTY: set controlling terminal for the session.
    if req == TIOCSCTTY {
        if let Some(FdBackend::PtySlave { pty_id }) = &backend {
            let calling_pid = crate::process::current_pid();
            let pty_id_val = *pty_id;
            let mut pt = crate::process::PROCESS_TABLE.lock();
            if let Some(proc) = pt.find_mut(calling_pid) {
                // Must be session leader with no controlling terminal.
                if proc.session_id != calling_pid || proc.controlling_tty.is_some() {
                    return NEG_EPERM;
                }
                proc.controlling_tty = Some(crate::process::ControllingTty::Pty(pty_id_val));
            }
            return 0;
        }
        return NEG_EINVAL;
    }

    // TIOCNOTTY: release controlling terminal.
    if req == TIOCNOTTY {
        let calling_pid = crate::process::current_pid();
        let mut pt = crate::process::PROCESS_TABLE.lock();
        if let Some(proc) = pt.find_mut(calling_pid) {
            proc.controlling_tty = None;
        }
        return 0;
    }

    match req {
        TCGETS => {
            if let Some(id) = pty_id {
                let table = crate::pty::PTY_TABLE.lock();
                if let Some(Some(pair)) = table.get(id as usize) {
                    let src = unsafe {
                        core::slice::from_raw_parts(
                            &pair.termios as *const _ as *const u8,
                            TERMIOS_SIZE,
                        )
                    };
                    if crate::mm::user_mem::copy_to_user(arg, src).is_err() {
                        return NEG_EFAULT;
                    }
                    return 0;
                }
                return NEG_EIO;
            }
            // Console TTY0.
            let tty = crate::tty::TTY0.lock();
            let src = unsafe {
                core::slice::from_raw_parts(&tty.termios as *const _ as *const u8, TERMIOS_SIZE)
            };
            if crate::mm::user_mem::copy_to_user(arg, src).is_err() {
                return NEG_EFAULT;
            }
            0
        }
        TCSETS | TCSETSW => {
            let mut buf = [0u8; TERMIOS_SIZE];
            if crate::mm::user_mem::copy_from_user(&mut buf, arg).is_err() {
                return NEG_EFAULT;
            }
            let new_termios = unsafe {
                core::ptr::read_unaligned(buf.as_ptr() as *const kernel_core::tty::Termios)
            };
            if let Some(id) = pty_id {
                let mut table = crate::pty::PTY_TABLE.lock();
                if let Some(Some(pair)) = table.get_mut(id as usize) {
                    pair.termios = new_termios;
                    return 0;
                }
                return NEG_EIO;
            }
            crate::tty::TTY0.lock().termios = new_termios;
            0
        }
        TCSETSF => {
            let mut buf = [0u8; TERMIOS_SIZE];
            if crate::mm::user_mem::copy_from_user(&mut buf, arg).is_err() {
                return NEG_EFAULT;
            }
            let new_termios = unsafe {
                core::ptr::read_unaligned(buf.as_ptr() as *const kernel_core::tty::Termios)
            };
            if let Some(id) = pty_id {
                let mut table = crate::pty::PTY_TABLE.lock();
                if let Some(Some(pair)) = table.get_mut(id as usize) {
                    pair.edit_buf.clear();
                    pair.m2s.clear();
                    pair.eof_pending = false;
                    pair.termios = new_termios;
                    return 0;
                }
                return NEG_EIO;
            }
            crate::stdin::flush();
            let mut tty = crate::tty::TTY0.lock();
            tty.edit_buf.clear();
            tty.termios = new_termios;
            0
        }
        TIOCGPGRP => {
            if let Some(id) = pty_id {
                let table = crate::pty::PTY_TABLE.lock();
                if let Some(Some(pair)) = table.get(id as usize) {
                    let pgid = pair.slave_fg_pgid;
                    let bytes = (pgid as i32).to_ne_bytes();
                    if crate::mm::user_mem::copy_to_user(arg, &bytes).is_err() {
                        return NEG_EFAULT;
                    }
                    return 0;
                }
                return NEG_EIO;
            }
            let tty = crate::tty::TTY0.lock();
            let pgid = tty.fg_pgid;
            let bytes = (pgid as i32).to_ne_bytes();
            if crate::mm::user_mem::copy_to_user(arg, &bytes).is_err() {
                return NEG_EFAULT;
            }
            0
        }
        TIOCSPGRP => {
            let mut bytes = [0u8; 4];
            if crate::mm::user_mem::copy_from_user(&mut bytes, arg).is_err() {
                return NEG_EFAULT;
            }
            let pgid = i32::from_ne_bytes(bytes) as u32;
            if let Some(id) = pty_id {
                let mut table = crate::pty::PTY_TABLE.lock();
                if let Some(Some(pair)) = table.get_mut(id as usize) {
                    pair.slave_fg_pgid = pgid;
                    return 0;
                }
                return NEG_EIO;
            }
            crate::tty::TTY0.lock().fg_pgid = pgid;
            crate::process::FG_PGID.store(pgid, core::sync::atomic::Ordering::Relaxed);
            0
        }
        TIOCGWINSZ => {
            if let Some(id) = pty_id {
                let table = crate::pty::PTY_TABLE.lock();
                if let Some(Some(pair)) = table.get(id as usize) {
                    let src = unsafe {
                        core::slice::from_raw_parts(
                            &pair.winsize as *const _ as *const u8,
                            WINSIZE_SIZE,
                        )
                    };
                    if crate::mm::user_mem::copy_to_user(arg, src).is_err() {
                        return NEG_EFAULT;
                    }
                    return 0;
                }
                return NEG_EIO;
            }
            let tty = crate::tty::TTY0.lock();
            let src = unsafe {
                core::slice::from_raw_parts(&tty.winsize as *const _ as *const u8, WINSIZE_SIZE)
            };
            if crate::mm::user_mem::copy_to_user(arg, src).is_err() {
                return NEG_EFAULT;
            }
            0
        }
        TIOCSWINSZ => {
            let mut buf = [0u8; WINSIZE_SIZE];
            if crate::mm::user_mem::copy_from_user(&mut buf, arg).is_err() {
                return NEG_EFAULT;
            }
            let new_ws = unsafe {
                core::ptr::read_unaligned(buf.as_ptr() as *const kernel_core::tty::Winsize)
            };
            if let Some(id) = pty_id {
                let mut table = crate::pty::PTY_TABLE.lock();
                if let Some(Some(pair)) = table.get_mut(id as usize) {
                    let changed = pair.winsize.ws_row != new_ws.ws_row
                        || pair.winsize.ws_col != new_ws.ws_col;
                    pair.winsize = new_ws;
                    let fg = pair.slave_fg_pgid;
                    drop(table);
                    if changed && fg != 0 {
                        crate::process::send_signal_to_group(fg, crate::process::SIGWINCH);
                    }
                    return 0;
                }
                return NEG_EIO;
            }
            let mut tty = crate::tty::TTY0.lock();
            let changed =
                tty.winsize.ws_row != new_ws.ws_row || tty.winsize.ws_col != new_ws.ws_col;
            tty.winsize = new_ws;
            let fg = tty.fg_pgid;
            drop(tty);
            if changed && fg != 0 {
                crate::process::send_signal_to_group(fg, crate::process::SIGWINCH);
            }
            0
        }
        _ => NEG_EINVAL,
    }
}

// ---------------------------------------------------------------------------
// T026: uname(buf) — writes a fixed struct utsname
// ---------------------------------------------------------------------------

fn sys_linux_uname(buf_ptr: u64) -> u64 {
    // struct utsname: 6 fields of 65 bytes each = 390 bytes
    let mut utsname = [0u8; 390];
    let fill = |dst: &mut [u8], s: &[u8]| {
        let n = s.len().min(dst.len() - 1);
        dst[..n].copy_from_slice(&s[..n]);
    };
    fill(&mut utsname[0..65], b"m3os"); // sysname
    fill(&mut utsname[65..130], b"m3os"); // nodename
    fill(&mut utsname[130..195], env!("CARGO_PKG_VERSION").as_bytes()); // release
    fill(&mut utsname[195..260], env!("CARGO_PKG_VERSION").as_bytes()); // version
    fill(&mut utsname[260..325], b"x86_64"); // machine
    // domainname left as zero
    if crate::mm::user_mem::copy_to_user(buf_ptr, &utsname).is_err() {
        return NEG_EFAULT;
    }
    0
}

// ---------------------------------------------------------------------------
// T026 (via path): newfstatat(dirfd, path, stat_ptr, flags)
// ---------------------------------------------------------------------------

fn sys_linux_fstatat(dirfd: u64, path_ptr: u64, stat_ptr: u64, flags: u64) -> u64 {
    let mut buf = [0u8; 512];
    let raw_name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    let lexical = match resolve_path_from_dirfd(dirfd, raw_name) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let resolved = match resolve_existing_fs_path(&lexical, flags & AT_SYMLINK_NOFOLLOW == 0) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let name: &str = &resolved;

    if let Some(st) = crate::fs::procfs::stat(name) {
        let mut stat = [0u8; 144];
        stat[8..16].copy_from_slice(&st.ino.to_ne_bytes());
        stat[16..24].copy_from_slice(&st.nlink.to_ne_bytes());
        stat[24..28].copy_from_slice(&st.mode.to_ne_bytes());
        stat[28..32].copy_from_slice(&st.uid.to_ne_bytes());
        stat[32..36].copy_from_slice(&st.gid.to_ne_bytes());
        stat[48..56].copy_from_slice(&st.size.to_ne_bytes());
        let blksize: u64 = 4096;
        stat[56..64].copy_from_slice(&blksize.to_ne_bytes());
        if crate::mm::user_mem::copy_to_user(stat_ptr, &stat).is_err() {
            return NEG_EFAULT;
        }
        return 0;
    }

    // Check tmpfs first.
    if let Some(rel) = tmpfs_relative_path(name) {
        let tmpfs = crate::fs::tmpfs::TMPFS.lock();
        let st = match tmpfs.stat(rel) {
            Ok(s) => s,
            Err(crate::fs::tmpfs::TmpfsError::NotFound) => return NEG_ENOENT,
            Err(crate::fs::tmpfs::TmpfsError::NotADirectory) => {
                return NEG_ENOTDIR;
            }
            Err(_) => return NEG_EINVAL,
        };
        let mode: u32 = if st.is_dir {
            0x4000 | st.mode as u32
        } else if st.is_symlink {
            0xA000 | 0o777
        } else {
            0x8000 | st.mode as u32
        };
        let mut stat = [0u8; 144];
        stat[8..16].copy_from_slice(&st.ino.to_ne_bytes());
        stat[16..24].copy_from_slice(&st.nlink.to_ne_bytes());
        stat[24..28].copy_from_slice(&mode.to_ne_bytes());
        stat[28..32].copy_from_slice(&st.uid.to_ne_bytes());
        stat[32..36].copy_from_slice(&st.gid.to_ne_bytes());
        let size = st.size as u64;
        stat[48..56].copy_from_slice(&size.to_ne_bytes());
        let blksize: u64 = 4096;
        stat[56..64].copy_from_slice(&blksize.to_ne_bytes());
        drop(tmpfs);
        if crate::mm::user_mem::copy_to_user(stat_ptr, &stat).is_err() {
            return NEG_EFAULT;
        }
        return 0;
    }

    // Check ramdisk tree (supports directories and hierarchical paths).
    match crate::fs::ramdisk::ramdisk_lookup(name) {
        Some(crate::fs::ramdisk::RamdiskNode::File { content }) => {
            let mut stat = [0u8; 144];
            let mode: u32 = 0x8000 | 0o755; // S_IFREG + executable (ramdisk binaries)
            stat[24..28].copy_from_slice(&mode.to_ne_bytes());
            let size = content.len() as u64;
            stat[48..56].copy_from_slice(&size.to_ne_bytes());
            let blksize: u64 = 4096;
            stat[56..64].copy_from_slice(&blksize.to_ne_bytes());
            if crate::mm::user_mem::copy_to_user(stat_ptr, &stat).is_err() {
                return NEG_EFAULT;
            }
            0
        }
        Some(crate::fs::ramdisk::RamdiskNode::Dir { .. }) => {
            let mut stat = [0u8; 144];
            let mode: u32 = 0x4000 | 0o755; // S_IFDIR
            stat[24..28].copy_from_slice(&mode.to_ne_bytes());
            let blksize: u64 = 4096;
            stat[56..64].copy_from_slice(&blksize.to_ne_bytes());
            if crate::mm::user_mem::copy_to_user(stat_ptr, &stat).is_err() {
                return NEG_EFAULT;
            }
            0
        }
        None => {
            // ext2 root filesystem: stat any path.
            if crate::fs::ext2::is_mounted()
                && let Some(rel) = ext2_root_path(name)
            {
                let vol = crate::fs::ext2::EXT2_VOLUME.lock();
                if let Some(vol) = vol.as_ref()
                    && let Ok(ino) = vol.resolve_path(rel)
                    && let Ok(inode) = vol.read_inode(ino)
                {
                    let mode = inode.mode as u32;
                    let uid = inode.uid as u32;
                    let gid = inode.gid as u32;
                    let size = inode.size as u64;
                    let nlink = inode.links_count as u64;
                    let blksize = vol.block_size as u64;
                    let ino = ino as u64;
                    let mut stat = [0u8; 144];
                    stat[8..16].copy_from_slice(&ino.to_ne_bytes());
                    // st_nlink at offset 16 (u64 on x86_64 stat)
                    stat[16..24].copy_from_slice(&nlink.to_ne_bytes());
                    stat[24..28].copy_from_slice(&mode.to_ne_bytes());
                    stat[28..32].copy_from_slice(&uid.to_ne_bytes());
                    stat[32..36].copy_from_slice(&gid.to_ne_bytes());
                    stat[48..56].copy_from_slice(&size.to_ne_bytes());
                    stat[56..64].copy_from_slice(&blksize.to_ne_bytes());
                    // Phase 32: populate timestamps from ext2 inode
                    let atime = inode.atime as i64;
                    let mtime = inode.mtime as i64;
                    let ctime = inode.ctime as i64;
                    stat[72..80].copy_from_slice(&atime.to_ne_bytes());
                    stat[88..96].copy_from_slice(&mtime.to_ne_bytes());
                    stat[104..112].copy_from_slice(&ctime.to_ne_bytes());
                    if crate::mm::user_mem::copy_to_user(stat_ptr, &stat).is_err() {
                        return NEG_EFAULT;
                    }
                    return 0;
                }
            }
            // Device special files.
            if name == "/dev/null"
                || name == "/dev/zero"
                || name == "/dev/urandom"
                || name == "/dev/random"
                || name == "/dev/full"
                || name == "/dev/ptmx"
                || name.starts_with("/dev/pts/")
            {
                let mut stat = [0u8; 144];
                let mode: u32 = 0x2000 | 0o666; // S_IFCHR | rw-rw-rw-
                stat[24..28].copy_from_slice(&mode.to_ne_bytes());
                if crate::mm::user_mem::copy_to_user(stat_ptr, &stat).is_err() {
                    return NEG_EFAULT;
                }
                return 0;
            }
            // Also handle "/" specially.
            if name == "/" {
                let mut stat = [0u8; 144];
                let mode: u32 = 0x4000 | 0o755;
                stat[24..28].copy_from_slice(&mode.to_ne_bytes());
                let blksize: u64 = 4096;
                stat[56..64].copy_from_slice(&blksize.to_ne_bytes());
                if crate::mm::user_mem::copy_to_user(stat_ptr, &stat).is_err() {
                    return NEG_EFAULT;
                }
                return 0;
            }
            NEG_ENOENT
        }
    }
}

fn sys_symlink(target_ptr: u64, linkpath_ptr: u64) -> u64 {
    sys_symlinkat(target_ptr, AT_FDCWD, linkpath_ptr)
}

fn sys_symlinkat(target_ptr: u64, dirfd: u64, linkpath_ptr: u64) -> u64 {
    let mut target_buf = [0u8; 4096];
    let target = match read_user_cstr(target_ptr, &mut target_buf) {
        Some(s) => s,
        None => return NEG_EFAULT,
    };

    let mut link_buf = [0u8; 512];
    let raw_link = match read_user_cstr(linkpath_ptr, &mut link_buf) {
        Some(s) => s,
        None => return NEG_EFAULT,
    };

    let lexical = match resolve_path_from_dirfd(dirfd, raw_link) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let resolved = match resolve_parent_components(&lexical) {
        Ok(path) => path,
        Err(err) => return err,
    };

    if path_node_nofollow(&resolved).is_ok() {
        return NEG_EEXIST;
    }

    if let Some((pu, pg, pm)) = parent_dir_metadata(&resolved) {
        let (_, _, euid, egid) = current_process_ids();
        if !check_permission(pu, pg, pm, euid, egid, 3) {
            return NEG_EACCES;
        }
    }
    if create_parent_is_read_only(&resolved) {
        return NEG_EROFS;
    }

    match resolve_fs_target(&resolved) {
        FsTarget::Tmpfs(rel) => {
            let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
            let (_, _, euid, egid) = current_process_ids();
            match tmpfs.create_symlink_with_meta(&rel, target, euid, egid) {
                Ok(()) => 0,
                Err(crate::fs::tmpfs::TmpfsError::AlreadyExists) => NEG_EEXIST,
                Err(crate::fs::tmpfs::TmpfsError::NotFound) => NEG_ENOENT,
                Err(crate::fs::tmpfs::TmpfsError::NotADirectory) => NEG_ENOTDIR,
                Err(_) => NEG_EIO,
            }
        }
        FsTarget::DiskData(rel) => {
            if !crate::fs::ext2::is_mounted() {
                return NEG_EROFS;
            }
            let mut vol = crate::fs::ext2::EXT2_VOLUME.lock();
            let Some(vol) = vol.as_mut() else {
                return NEG_EIO;
            };
            let parts: alloc::vec::Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
            if parts.is_empty() {
                return NEG_EINVAL;
            }
            let (parent_ino, link_name) = if parts.len() == 1 {
                (kernel_core::fs::ext2::EXT2_ROOT_INO, parts[0])
            } else {
                let parent_rel = parts[..parts.len() - 1].join("/");
                match vol.resolve_path(&parent_rel) {
                    Ok(ino) => (ino, parts[parts.len() - 1]),
                    Err(kernel_core::fs::ext2::Ext2Error::NotDirectory) => return NEG_ENOTDIR,
                    Err(kernel_core::fs::ext2::Ext2Error::NotFound) => return NEG_ENOENT,
                    Err(_) => return NEG_EIO,
                }
            };
            let (_, _, euid, egid) = current_process_ids();
            match vol.create_symlink(parent_ino, link_name, target, euid, egid) {
                Ok(_) => 0,
                Err(kernel_core::fs::ext2::Ext2Error::AlreadyExists) => NEG_EEXIST,
                Err(kernel_core::fs::ext2::Ext2Error::NotDirectory) => NEG_ENOTDIR,
                Err(kernel_core::fs::ext2::Ext2Error::NotFound) => NEG_ENOENT,
                Err(kernel_core::fs::ext2::Ext2Error::OutOfSpace) => NEG_ENOSPC,
                Err(_) => NEG_EIO,
            }
        }
        FsTarget::Ramdisk => NEG_EROFS,
    }
}

fn sys_readlink(path_ptr: u64, buf_ptr: u64, buf_len: u64) -> u64 {
    sys_readlinkat(AT_FDCWD, path_ptr, buf_ptr, buf_len)
}

fn sys_readlinkat(dirfd: u64, path_ptr: u64, buf_ptr: u64, buf_len: u64) -> u64 {
    if buf_len == 0 {
        return NEG_EINVAL;
    }
    let mut path_buf = [0u8; 512];
    let raw_path = match read_user_cstr(path_ptr, &mut path_buf) {
        Some(s) => s,
        None => return NEG_EFAULT,
    };
    let lexical = match resolve_path_from_dirfd(dirfd, raw_path) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let resolved = match resolve_existing_fs_path(&lexical, false) {
        Ok(path) => path,
        Err(err) => return err,
    };

    let target = match path_node_nofollow(&resolved) {
        Ok(PathNodeKind::Symlink(target)) => target,
        Ok(_) => return NEG_EINVAL,
        Err(err) => return err,
    };

    let to_copy = core::cmp::min(target.len(), buf_len as usize);
    if crate::mm::user_mem::copy_to_user(buf_ptr, &target.as_bytes()[..to_copy]).is_err() {
        return NEG_EFAULT;
    }
    to_copy as u64
}

fn sys_link(oldpath_ptr: u64, newpath_ptr: u64) -> u64 {
    sys_linkat(AT_FDCWD, oldpath_ptr, AT_FDCWD, newpath_ptr, 0)
}

fn sys_linkat(olddirfd: u64, oldpath_ptr: u64, newdirfd: u64, newpath_ptr: u64, flags: u64) -> u64 {
    if flags & !AT_SYMLINK_FOLLOW != 0 {
        return NEG_EINVAL;
    }
    let mut old_buf = [0u8; 512];
    let raw_old = match read_user_cstr(oldpath_ptr, &mut old_buf) {
        Some(path) => path,
        None => return NEG_EFAULT,
    };
    let mut new_buf = [0u8; 512];
    let raw_new = match read_user_cstr(newpath_ptr, &mut new_buf) {
        Some(path) => path,
        None => return NEG_EFAULT,
    };

    let old_lexical = match resolve_path_from_dirfd(olddirfd, raw_old) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let follow_old = flags & AT_SYMLINK_FOLLOW != 0;
    let old_resolved = match resolve_existing_fs_path(&old_lexical, follow_old) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let new_lexical = match resolve_path_from_dirfd(newdirfd, raw_new) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let new_resolved = match resolve_parent_components(&new_lexical) {
        Ok(path) => path,
        Err(err) => return err,
    };

    if path_node_nofollow(&new_resolved).is_ok() {
        return NEG_EEXIST;
    }
    if let Some((pu, pg, pm)) = parent_dir_metadata(&new_resolved) {
        let (_, _, euid, egid) = current_process_ids();
        if !check_permission(pu, pg, pm, euid, egid, 3) {
            return NEG_EACCES;
        }
    }
    if create_parent_is_read_only(&new_resolved) {
        return NEG_EROFS;
    }

    let old_target = resolve_fs_target(&old_resolved);
    let new_target = resolve_fs_target(&new_resolved);
    match (&old_target, &new_target) {
        (FsTarget::DiskData(_), FsTarget::DiskData(_)) => {}
        (FsTarget::DiskData(_), _) | (_, FsTarget::DiskData(_)) => return NEG_EXDEV,
        _ => return NEG_EROFS,
    }
    if !crate::fs::ext2::is_mounted() {
        return NEG_EROFS;
    }

    let FsTarget::DiskData(old_rel) = old_target else {
        return NEG_EROFS;
    };
    let FsTarget::DiskData(new_rel) = new_target else {
        return NEG_EROFS;
    };

    let mut vol = crate::fs::ext2::EXT2_VOLUME.lock();
    let Some(vol) = vol.as_mut() else {
        return NEG_EIO;
    };
    let old_ino = match vol.resolve_path(&old_rel) {
        Ok(ino) => ino,
        Err(kernel_core::fs::ext2::Ext2Error::NotFound) => return NEG_ENOENT,
        Err(kernel_core::fs::ext2::Ext2Error::NotDirectory) => return NEG_ENOTDIR,
        Err(_) => return NEG_EIO,
    };
    let old_inode = match vol.read_inode(old_ino) {
        Ok(inode) => inode,
        Err(_) => return NEG_EIO,
    };
    if old_inode.is_dir() {
        return NEG_EPERM;
    }

    let parts: alloc::vec::Vec<&str> = new_rel.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return NEG_EEXIST;
    }
    let (parent_ino, link_name) = if parts.len() == 1 {
        (kernel_core::fs::ext2::EXT2_ROOT_INO, parts[0])
    } else {
        let parent_path = parts[..parts.len() - 1].join("/");
        match vol.resolve_path(&parent_path) {
            Ok(ino) => (ino, parts[parts.len() - 1]),
            Err(kernel_core::fs::ext2::Ext2Error::NotFound) => return NEG_ENOENT,
            Err(kernel_core::fs::ext2::Ext2Error::NotDirectory) => return NEG_ENOTDIR,
            Err(_) => return NEG_EIO,
        }
    };

    match vol.create_hard_link(parent_ino, link_name, old_ino) {
        Ok(()) => 0,
        Err(kernel_core::fs::ext2::Ext2Error::AlreadyExists) => NEG_EEXIST,
        Err(kernel_core::fs::ext2::Ext2Error::IsDirectory) => NEG_EPERM,
        Err(kernel_core::fs::ext2::Ext2Error::NotDirectory) => NEG_ENOTDIR,
        Err(kernel_core::fs::ext2::Ext2Error::NotFound) => NEG_ENOENT,
        Err(kernel_core::fs::ext2::Ext2Error::OutOfSpace) => NEG_ENOSPC,
        Err(_) => NEG_EIO,
    }
}

// ---------------------------------------------------------------------------
// Phase 32: utimensat(dirfd, path, times, flags) — syscall 280
// ---------------------------------------------------------------------------

/// Get approximate current Unix timestamp from LAPIC tick counter.
fn current_unix_time() -> u32 {
    let ticks = crate::arch::x86_64::interrupts::tick_count();
    (ticks / TICKS_PER_SEC) as u32
}

fn sys_utimensat(_dirfd: u64, path_ptr: u64, times_ptr: u64, _flags: u64) -> u64 {
    let mut buf = [0u8; 512];
    let raw_name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    let cwd = current_cwd();
    let resolved = resolve_path(&cwd, raw_name);
    let name: &str = &resolved;

    // Read the times array if provided.
    // struct timespec { tv_sec: i64, tv_nsec: i64 } × 2 = 32 bytes
    // times[0] = atime, times[1] = mtime
    // UTIME_NOW = 0x3FFFFFFF, UTIME_OMIT = 0x3FFFFFFE
    const UTIME_NOW: i64 = 0x3FFFFFFF;
    const UTIME_OMIT: i64 = 0x3FFFFFFE;

    let now = current_unix_time();
    let (new_atime, new_mtime) = if times_ptr == 0 {
        // NULL times → set both to current time
        (now, now)
    } else {
        let mut tbuf = [0u8; 32];
        if crate::mm::user_mem::copy_from_user(&mut tbuf, times_ptr).is_err() {
            return NEG_EFAULT;
        }
        let a_sec = i64::from_ne_bytes(tbuf[0..8].try_into().unwrap());
        let a_nsec = i64::from_ne_bytes(tbuf[8..16].try_into().unwrap());
        let m_sec = i64::from_ne_bytes(tbuf[16..24].try_into().unwrap());
        let m_nsec = i64::from_ne_bytes(tbuf[24..32].try_into().unwrap());

        let atime = if a_nsec == UTIME_NOW {
            now
        } else if a_nsec == UTIME_OMIT {
            u32::MAX // sentinel: don't change
        } else {
            // Validate timespec: tv_sec >= 0, tv_sec fits u32, tv_nsec in [0, 1e9)
            // Reject u32::MAX (collides with internal OMIT sentinel)
            if a_sec < 0 || a_sec >= u32::MAX as i64 || !(0..1_000_000_000).contains(&a_nsec) {
                return NEG_EINVAL;
            }
            a_sec as u32
        };
        let mtime = if m_nsec == UTIME_NOW {
            now
        } else if m_nsec == UTIME_OMIT {
            u32::MAX // sentinel: don't change
        } else {
            if m_sec < 0 || m_sec >= u32::MAX as i64 || !(0..1_000_000_000).contains(&m_nsec) {
                return NEG_EINVAL;
            }
            m_sec as u32
        };
        (atime, mtime)
    };

    // ext2 root filesystem
    if crate::fs::ext2::is_mounted()
        && let Some(rel) = ext2_root_path(name)
    {
        let mut vol = crate::fs::ext2::EXT2_VOLUME.lock();
        if let Some(vol) = vol.as_mut()
            && let Ok(ino) = vol.resolve_path(rel)
            && let Ok(mut inode) = vol.read_inode(ino)
        {
            if new_atime != u32::MAX {
                inode.atime = new_atime;
            }
            if new_mtime != u32::MAX {
                inode.mtime = new_mtime;
            }
            if new_atime != u32::MAX || new_mtime != u32::MAX {
                inode.ctime = now; // ctime always updated when any timestamp changes
            }
            if vol.write_inode(ino, &inode).is_err() {
                return NEG_EIO;
            }
            return 0;
        }
        return NEG_ENOENT;
    }

    // tmpfs
    if tmpfs_relative_path(name).is_some() {
        // tmpfs doesn't track timestamps yet — return ENOSYS
        return NEG_ENOSYS;
    }

    NEG_ENOENT
}

// ---------------------------------------------------------------------------
// Phase 13: mkdir(pathname) — syscall 83
// ---------------------------------------------------------------------------

fn sys_linux_mkdir(path_ptr: u64, mode: u64) -> u64 {
    let mut buf = [0u8; 512];
    let raw_name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    // mkdir() should resolve parent symlinks but operate on the lexical basename.
    let lexical = match resolve_path_from_dirfd(AT_FDCWD, raw_name) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let resolved = match resolve_parent_components(&lexical) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let name: &str = &resolved;

    // Phase 27: Write+execute permission on parent directory.
    if let Some((pu, pg, pm)) = parent_dir_metadata(name) {
        let (_, _, euid, egid) = current_process_ids();
        if !check_permission(pu, pg, pm, euid, egid, 3) {
            return NEG_EACCES;
        }
    }

    // Phase 28: ext2 root mkdir.
    if crate::fs::ext2::is_mounted()
        && let Some(rel) = ext2_root_path(name)
        && !rel.is_empty()
    {
        let mut vol = crate::fs::ext2::EXT2_VOLUME.lock();
        if let Some(vol) = vol.as_mut() {
            let parts: alloc::vec::Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
            let (parent_ino, dir_name) = if parts.len() <= 1 {
                (kernel_core::fs::ext2::EXT2_ROOT_INO, rel)
            } else {
                let parent_path = parts[..parts.len() - 1].join("/");
                match vol.resolve_path(&parent_path) {
                    Ok(p) => (p, parts[parts.len() - 1]),
                    Err(_) => return NEG_ENOENT,
                }
            };
            let (_, _, mk_euid, mk_egid) = current_process_ids();
            let create_mode = ((mode as u16) & 0o7777) & !current_umask();
            return match vol.create_directory(parent_ino, dir_name, create_mode, mk_euid, mk_egid) {
                Ok(_) => {
                    log::info!("[mkdir] {} (ext2)", name);
                    0
                }
                Err(kernel_core::fs::ext2::Ext2Error::AlreadyExists) => NEG_EEXIST,
                Err(_) => NEG_EIO,
            };
        }
        return NEG_EIO;
    }

    // Legacy: /data mkdir (ext2 or FAT32 fallback).
    if let Some(rel) = fat32_relative_path(name) {
        if rel.is_empty() {
            return NEG_EINVAL;
        }
        if crate::fs::ext2::is_mounted() {
            let mut vol = crate::fs::ext2::EXT2_VOLUME.lock();
            if let Some(vol) = vol.as_mut() {
                let parts: alloc::vec::Vec<&str> =
                    rel.split('/').filter(|s| !s.is_empty()).collect();
                let (parent_ino, dir_name) = if parts.len() <= 1 {
                    (kernel_core::fs::ext2::EXT2_ROOT_INO, rel)
                } else {
                    let parent_path = parts[..parts.len() - 1].join("/");
                    match vol.resolve_path(&parent_path) {
                        Ok(p) => (p, parts[parts.len() - 1]),
                        Err(_) => return NEG_ENOENT,
                    }
                };
                let (_, _, mk_euid, mk_egid) = current_process_ids();
                let create_mode = ((mode as u16) & 0o7777) & !current_umask();
                return match vol.create_directory(
                    parent_ino,
                    dir_name,
                    create_mode,
                    mk_euid,
                    mk_egid,
                ) {
                    Ok(_) => {
                        log::info!("[mkdir] {} (ext2)", name);
                        0
                    }
                    Err(kernel_core::fs::ext2::Ext2Error::AlreadyExists) => NEG_EEXIST,
                    Err(_) => NEG_EIO,
                };
            }
            return NEG_EIO;
        }
        if crate::fs::fat32::is_mounted() {
            let mut vol = crate::fs::fat32::FAT32_VOLUME.lock();
            if let Some(vol) = vol.as_mut() {
                let parts: alloc::vec::Vec<&str> =
                    rel.split('/').filter(|s| !s.is_empty()).collect();
                let (parent_cluster, dir_name) = if parts.len() <= 1 {
                    (vol.bpb.root_cluster, rel)
                } else {
                    let parent_path = parts[..parts.len() - 1].join("/");
                    let parent_cluster = match vol.lookup(&parent_path) {
                        Ok(pe) if pe.is_dir() => pe.start_cluster(),
                        _ => return NEG_ENOENT,
                    };
                    (parent_cluster, parts[parts.len() - 1])
                };
                return match vol.mkdir(parent_cluster, dir_name) {
                    Ok(_) => {
                        log::info!("[mkdir] {} (fat32)", name);
                        let (_, _, mk_euid2, mk_egid2) = current_process_ids();
                        let create_mode = ((mode as u16) & 0o7777) & !current_umask();
                        crate::fs::fat32::set_fat32_meta(rel, mk_euid2, mk_egid2, create_mode);
                        0
                    }
                    Err(kernel_core::fs::fat32::Fat32Error::AlreadyExists) => NEG_EEXIST,
                    Err(_) => NEG_EIO,
                };
            }
        }
        return NEG_ENOENT;
    }

    let rel = match tmpfs_relative_path(name) {
        Some(r) => r,
        None => return NEG_EROFS, // can only mkdir in tmpfs or /data
    };
    if rel.is_empty() {
        return NEG_EINVAL; // can't mkdir /tmp itself
    }

    let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
    let (_, _, mk_euid, mk_egid) = current_process_ids();
    let create_mode = ((mode as u16) & 0o7777) & !current_umask();
    match tmpfs.mkdir_with_meta(rel, mk_euid, mk_egid, create_mode) {
        Ok(()) => {
            log::info!("[mkdir] {}", name);
            0
        }
        Err(crate::fs::tmpfs::TmpfsError::AlreadyExists) => NEG_EEXIST,
        Err(crate::fs::tmpfs::TmpfsError::NotFound) => NEG_ENOENT,
        Err(crate::fs::tmpfs::TmpfsError::NotADirectory) => NEG_ENOTDIR,
        Err(_) => NEG_EINVAL,
    }
}

// ---------------------------------------------------------------------------
// Phase 13: rmdir(pathname) — syscall 84
// ---------------------------------------------------------------------------

fn sys_linux_rmdir(path_ptr: u64) -> u64 {
    let mut buf = [0u8; 512];
    let raw_name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    let cwd = current_cwd();
    let resolved = resolve_path(&cwd, raw_name);
    let name: &str = &resolved;

    // Phase 27: Write+execute permission on parent directory.
    if let Some((pu, pg, pm)) = parent_dir_metadata(name) {
        let (_, _, euid, egid) = current_process_ids();
        if !check_permission(pu, pg, pm, euid, egid, 3) {
            return NEG_EACCES;
        }
    }

    let rel = match tmpfs_relative_path(name) {
        Some(r) => r,
        None => return NEG_EROFS,
    };
    if rel.is_empty() {
        return NEG_EINVAL; // can't rmdir /tmp itself
    }

    let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
    match tmpfs.rmdir(rel) {
        Ok(()) => {
            log::info!("[rmdir] {}", name);
            0
        }
        Err(crate::fs::tmpfs::TmpfsError::NotEmpty) => NEG_ENOTEMPTY,
        Err(crate::fs::tmpfs::TmpfsError::NotFound) => NEG_ENOENT,
        Err(
            crate::fs::tmpfs::TmpfsError::WrongType | crate::fs::tmpfs::TmpfsError::NotADirectory,
        ) => NEG_ENOTDIR,
        Err(_) => NEG_EINVAL,
    }
}

// ---------------------------------------------------------------------------
// Phase 13: unlink(pathname) — syscall 87
// ---------------------------------------------------------------------------

fn sys_linux_unlink(path_ptr: u64) -> u64 {
    let mut buf = [0u8; 512];
    let raw_name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    // unlink() should resolve parent symlinks but unlink the lexical final component.
    let lexical = match resolve_path_from_dirfd(AT_FDCWD, raw_name) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let resolved = match resolve_parent_components(&lexical) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let name: &str = &resolved;

    // Phase 27: Write+execute permission on parent directory.
    if let Some((pu, pg, pm)) = parent_dir_metadata(name) {
        let (_, _, euid, egid) = current_process_ids();
        if !check_permission(pu, pg, pm, euid, egid, 3) {
            return NEG_EACCES;
        }
    }

    // Phase 28: ext2 root unlink.
    if crate::fs::ext2::is_mounted()
        && let Some(rel) = ext2_root_path(name)
        && !rel.is_empty()
    {
        let mut vol = crate::fs::ext2::EXT2_VOLUME.lock();
        if let Some(vol) = vol.as_mut() {
            let parts: alloc::vec::Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
            let parent_ino = if parts.len() <= 1 {
                kernel_core::fs::ext2::EXT2_ROOT_INO
            } else {
                let parent_path = parts[..parts.len() - 1].join("/");
                match vol.resolve_path(&parent_path) {
                    Ok(p) => p,
                    Err(_) => return NEG_ENOENT,
                }
            };
            let file_name = parts.last().copied().unwrap_or(rel);
            return match vol.delete_file(parent_ino, file_name) {
                Ok(()) => {
                    log::info!("[unlink] {} (ext2)", name);
                    0
                }
                Err(kernel_core::fs::ext2::Ext2Error::NotFound) => NEG_ENOENT,
                Err(kernel_core::fs::ext2::Ext2Error::IsDirectory) => NEG_EISDIR,
                Err(_) => NEG_EIO,
            };
        }
        return NEG_EIO;
    }

    // Legacy: /data unlink (ext2 or FAT32 fallback).
    if let Some(rel) = fat32_relative_path(name) {
        if rel.is_empty() {
            return NEG_EINVAL;
        }
        if crate::fs::ext2::is_mounted() {
            let mut vol = crate::fs::ext2::EXT2_VOLUME.lock();
            if let Some(vol) = vol.as_mut() {
                let parts: alloc::vec::Vec<&str> =
                    rel.split('/').filter(|s| !s.is_empty()).collect();
                let parent_ino = if parts.len() <= 1 {
                    kernel_core::fs::ext2::EXT2_ROOT_INO
                } else {
                    let parent_path = parts[..parts.len() - 1].join("/");
                    match vol.resolve_path(&parent_path) {
                        Ok(p) => p,
                        Err(_) => return NEG_ENOENT,
                    }
                };
                let file_name = parts.last().copied().unwrap_or(rel);
                return match vol.delete_file(parent_ino, file_name) {
                    Ok(()) => {
                        log::info!("[unlink] {} (ext2)", name);
                        0
                    }
                    Err(kernel_core::fs::ext2::Ext2Error::NotFound) => NEG_ENOENT,
                    Err(kernel_core::fs::ext2::Ext2Error::IsDirectory) => NEG_EISDIR,
                    Err(_) => NEG_EIO,
                };
            }
            return NEG_EIO;
        }
        if crate::fs::fat32::is_mounted() {
            let mut vol = crate::fs::fat32::FAT32_VOLUME.lock();
            if let Some(vol) = vol.as_mut() {
                let parts: alloc::vec::Vec<&str> =
                    rel.split('/').filter(|s| !s.is_empty()).collect();
                let (parent_cluster, file_name) = if parts.len() <= 1 {
                    (vol.bpb.root_cluster, rel)
                } else {
                    let parent_path = parts[..parts.len() - 1].join("/");
                    let parent_cluster = match vol.lookup(&parent_path) {
                        Ok(pe) if pe.is_dir() => pe.start_cluster(),
                        _ => return NEG_ENOENT,
                    };
                    (parent_cluster, parts[parts.len() - 1])
                };
                return match vol.unlink(parent_cluster, file_name) {
                    Ok(()) => {
                        log::info!("[unlink] {} (fat32)", name);
                        0
                    }
                    Err(kernel_core::fs::fat32::Fat32Error::NotFound) => NEG_ENOENT,
                    Err(kernel_core::fs::fat32::Fat32Error::IsDir) => NEG_EISDIR,
                    Err(_) => NEG_EIO,
                };
            }
        }
        return NEG_ENOENT;
    }

    let rel = match tmpfs_relative_path(name) {
        Some(r) => r,
        None => return NEG_EROFS,
    };
    if rel.is_empty() {
        return NEG_EINVAL;
    }

    let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
    match tmpfs.unlink(rel) {
        Ok(()) => {
            log::info!("[unlink] {}", name);
            0
        }
        Err(crate::fs::tmpfs::TmpfsError::NotFound) => NEG_ENOENT,
        Err(crate::fs::tmpfs::TmpfsError::WrongType) => NEG_EISDIR,
        Err(crate::fs::tmpfs::TmpfsError::NotADirectory) => NEG_ENOTDIR,
        Err(_) => NEG_EINVAL,
    }
}

// ---------------------------------------------------------------------------
// Phase 13: rename(oldpath, newpath) — syscall 82
// ---------------------------------------------------------------------------

fn sys_linux_rename(old_ptr: u64, new_ptr: u64) -> u64 {
    let mut buf1 = [0u8; 512];
    let old_raw = match read_user_cstr(old_ptr, &mut buf1) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };
    // Copy old_raw to owned string since we need buf for new_raw too.
    let mut old_owned = [0u8; 512];
    let old_len = old_raw.len();
    old_owned[..old_len].copy_from_slice(old_raw.as_bytes());

    let mut buf2 = [0u8; 512];
    let new_raw = match read_user_cstr(new_ptr, &mut buf2) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    let old_str_raw = core::str::from_utf8(&old_owned[..old_len]).unwrap();
    let old_lexical = match resolve_path_from_dirfd(AT_FDCWD, old_str_raw) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let new_lexical = match resolve_path_from_dirfd(AT_FDCWD, new_raw) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let old_resolved = match resolve_parent_components(&old_lexical) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let new_resolved = match resolve_parent_components(&new_lexical) {
        Ok(path) => path,
        Err(err) => return err,
    };

    // Phase 27: Write+execute permission on both parent directories.
    {
        let (_, _, euid, egid) = current_process_ids();
        if let Some((pu, pg, pm)) = parent_dir_metadata(&old_resolved)
            && !check_permission(pu, pg, pm, euid, egid, 3)
        {
            return NEG_EACCES;
        }
        if let Some((pu, pg, pm)) = parent_dir_metadata(&new_resolved)
            && !check_permission(pu, pg, pm, euid, egid, 3)
        {
            return NEG_EACCES;
        }
    }

    let old_rel = match tmpfs_relative_path(&old_resolved) {
        Some(r) => r,
        None => return NEG_EROFS,
    };
    let new_rel = match tmpfs_relative_path(&new_resolved) {
        Some(r) => r,
        None => return NEG_EROFS,
    };

    let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
    match tmpfs.rename(old_rel, new_rel) {
        Ok(()) => {
            log::info!("[rename] {} → {}", old_resolved, new_resolved);
            0
        }
        Err(crate::fs::tmpfs::TmpfsError::NotFound) => NEG_ENOENT,
        Err(_) => NEG_EINVAL,
    }
}

// ---------------------------------------------------------------------------
// Phase 24: mount(source, target, fstype) — syscall 165
// ---------------------------------------------------------------------------

fn sys_linux_mount(_source_ptr: u64, target_ptr: u64, fstype_ptr: u64) -> u64 {
    let _mount_guard = MOUNT_OP_LOCK.lock();
    let mut buf_target = [0u8; 512];
    let target = match read_user_cstr(target_ptr, &mut buf_target) {
        Some(s) => s,
        None => return NEG_EFAULT,
    };

    let mut buf_fstype = [0u8; 512];
    let fstype = match read_user_cstr(fstype_ptr, &mut buf_fstype) {
        Some(s) => s,
        None => return NEG_EFAULT,
    };

    let cwd = current_cwd();
    let resolved_target = resolve_path(&cwd, target);

    if fstype != "vfat" && fstype != "ext2" {
        log::warn!("[mount] unsupported fstype: {}", fstype);
        return NEG_EINVAL;
    }

    // Support mounting at / (ext2 root) or /data (legacy).
    if resolved_target != "/" && resolved_target != "/data" {
        log::warn!(
            "[mount] unsupported mountpoint {}; only / and /data are supported",
            resolved_target
        );
        return NEG_EINVAL;
    }

    // vfat can only mount at /data, not /.
    if fstype == "vfat" && resolved_target == "/" {
        log::warn!("[mount] vfat cannot be mounted at /; only /data is supported for vfat");
        return NEG_EINVAL;
    }

    // ext2 can only mount at /, not /data.
    if fstype == "ext2" && resolved_target == "/data" {
        log::warn!("[mount] ext2 cannot be mounted at /data; only / is supported for ext2");
        return NEG_EINVAL;
    }

    if fstype == "ext2" {
        let (base_lba, _) = match crate::blk::mbr::probe_ext2() {
            Some(p) => p,
            None => {
                log::error!("[mount] no ext2 partition found on virtio-blk");
                const NEG_ENODEV: u64 = (-19_i64) as u64;
                return NEG_ENODEV;
            }
        };
        match crate::fs::ext2::mount_ext2(base_lba) {
            Ok(()) => {
                log::info!("[mount] virtio-blk mounted at {} (ext2)", resolved_target);
                0
            }
            Err(e) => {
                log::error!("[mount] ext2 mount failed: {:?}", e);
                NEG_EIO
            }
        }
    } else {
        // fstype == "vfat"
        let (base_lba, _sector_count) = match crate::blk::mbr::probe() {
            Some(p) => p,
            None => {
                log::error!("[mount] no FAT32 partition found on virtio-blk");
                const NEG_ENODEV: u64 = (-19_i64) as u64;
                return NEG_ENODEV;
            }
        };
        match crate::fs::fat32::mount_fat32(base_lba) {
            Ok(()) => {
                log::info!(
                    "[mount] {} mounted at {} (vfat)",
                    "virtio-blk",
                    resolved_target
                );
                0
            }
            Err(e) => {
                log::error!("[mount] FAT32 mount failed: {:?}", e);
                NEG_EIO
            }
        }
    }
}

fn sys_linux_umount2(target_ptr: u64, flags: u64) -> u64 {
    let _mount_guard = MOUNT_OP_LOCK.lock();
    if flags != 0 {
        return NEG_EINVAL;
    }

    let (_, _, euid, _) = current_process_ids();
    if euid != 0 {
        return NEG_EPERM;
    }

    let mut buf_target = [0u8; 512];
    let target = match read_user_cstr(target_ptr, &mut buf_target) {
        Some(s) => s,
        None => return NEG_EFAULT,
    };

    let cwd = current_cwd();
    let resolved_target = resolve_path(&cwd, target);
    if resolved_target != "/" && resolved_target != "/data" {
        return NEG_EINVAL;
    }

    let table = crate::process::PROCESS_TABLE.lock();
    let busy = table.iter().any(|proc| {
        mount_contains_path(&resolved_target, &proc.cwd)
            || proc
                .fd_table
                .iter()
                .flatten()
                .any(|entry| mount_holds_fd(&resolved_target, &entry.backend))
    });
    drop(table);
    if busy {
        return NEG_EBUSY;
    }

    match resolved_target.as_str() {
        "/" => {
            if !crate::fs::ext2::is_mounted() {
                return NEG_EINVAL;
            }
            crate::fs::ext2::unmount_ext2();
        }
        "/data" => {
            if !crate::fs::fat32::is_mounted() {
                return NEG_EINVAL;
            }
            crate::fs::fat32::unmount_fat32();
        }
        _ => return NEG_EINVAL,
    }

    log::info!("[mount] unmounted {}", resolved_target);
    0
}

fn mount_contains_path(target: &str, path: &str) -> bool {
    match target {
        "/" => {
            if path == "/tmp"
                || path.starts_with("/tmp/")
                || path == "/proc"
                || path.starts_with("/proc/")
                || path == "/dev"
                || path.starts_with("/dev/")
            {
                return false;
            }
            crate::fs::ramdisk::ramdisk_lookup(path).is_none()
        }
        "/data" => path == "/data" || path.starts_with("/data/"),
        _ => false,
    }
}

fn mount_holds_fd(target: &str, backend: &FdBackend) -> bool {
    match (target, backend) {
        ("/", FdBackend::Ext2Disk { .. }) | ("/data", FdBackend::Fat32Disk { .. }) => true,
        (_, FdBackend::Dir { path }) => mount_contains_path(target, path),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Phase 13: truncate(path, length) — syscall 76
// ---------------------------------------------------------------------------

fn sys_linux_truncate(path_ptr: u64, length: u64) -> u64 {
    // Linux truncate() takes a signed off_t.
    let length_i64 = length as i64;
    if length_i64 < 0 {
        return NEG_EINVAL;
    }

    let mut buf = [0u8; 512];
    let raw_name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    // Resolve path against current process's working directory.
    let cwd = current_cwd();
    let resolved = resolve_path(&cwd, raw_name);
    let name: &str = &resolved;

    let rel = match tmpfs_relative_path(name) {
        Some(r) => r,
        None => return NEG_EROFS,
    };
    if rel.is_empty() {
        return NEG_EISDIR;
    }

    let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
    match tmpfs.truncate(rel, length_i64 as usize) {
        Ok(()) => 0,
        Err(crate::fs::tmpfs::TmpfsError::NotFound) => NEG_ENOENT,
        Err(crate::fs::tmpfs::TmpfsError::NoSpace) => NEG_ENOSPC,
        Err(crate::fs::tmpfs::TmpfsError::WrongType) => NEG_EISDIR,
        Err(_) => NEG_EINVAL,
    }
}

// ---------------------------------------------------------------------------
// Phase 13: ftruncate(fd, length) — syscall 77
// ---------------------------------------------------------------------------

fn sys_linux_ftruncate(fd: u64, length: u64) -> u64 {
    // Linux ftruncate() takes a signed off_t.
    let length_i64 = length as i64;
    if length_i64 < 0 {
        return NEG_EINVAL;
    }

    let fd_idx = fd as usize;
    if !(3..MAX_FDS).contains(&fd_idx) {
        return NEG_EBADF;
    }

    let entry = match current_fd_entry(fd_idx) {
        Some(e) => e,
        None => return NEG_EBADF,
    };

    if !entry.writable {
        return NEG_EBADF;
    }

    match &entry.backend {
        FdBackend::Stdout
        | FdBackend::Stdin
        | FdBackend::PipeRead { .. }
        | FdBackend::PipeWrite { .. }
        | FdBackend::Dir { .. }
        | FdBackend::DevNull
        | FdBackend::DevZero
        | FdBackend::DevUrandom
        | FdBackend::DevFull
        | FdBackend::DeviceTTY { .. }
        | FdBackend::PtyMaster { .. }
        | FdBackend::PtySlave { .. }
        | FdBackend::Proc { .. }
        | FdBackend::Socket { .. }
        | FdBackend::UnixSocket { .. }
        | FdBackend::Epoll { .. } => NEG_EINVAL,
        FdBackend::Ramdisk { .. } => NEG_EROFS,
        FdBackend::Tmpfs { path } => {
            let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
            match tmpfs.truncate(path, length_i64 as usize) {
                Ok(()) => 0,
                Err(crate::fs::tmpfs::TmpfsError::NoSpace) => NEG_ENOSPC,
                Err(_) => NEG_EINVAL,
            }
        }
        FdBackend::Fat32Disk { .. } | FdBackend::Ext2Disk { .. } => {
            // FAT32/ext2 truncate not yet implemented.
            NEG_EINVAL
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 13: fsync(fd) — syscall 74 (no-op for tmpfs)
// ---------------------------------------------------------------------------

fn sys_linux_fsync(fd: u64) -> u64 {
    let fd_idx = fd as usize;
    if !(3..MAX_FDS).contains(&fd_idx) {
        return NEG_EBADF;
    }
    if current_fd_entry(fd_idx).is_none() {
        return NEG_EBADF;
    }
    0 // no-op: tmpfs has no persistence
}

// ---------------------------------------------------------------------------
// Phase 13: getdents64(fd, buf, count) — syscall 217
// ---------------------------------------------------------------------------

fn sys_linux_getdents64(fd: u64, buf_ptr: u64, count: u64) -> u64 {
    let fd_idx = fd as usize;
    if fd_idx >= MAX_FDS {
        return NEG_EBADF;
    }

    let entry = match current_fd_entry(fd_idx) {
        Some(e) => e,
        None => return NEG_EBADF,
    };

    let dir_path = match &entry.backend {
        FdBackend::Dir { path } => path.clone(),
        _ => return NEG_ENOTDIR,
    };
    if let Some((uid, gid, mode)) = path_metadata(&dir_path) {
        let (_, _, euid, egid) = current_process_ids();
        if !check_permission(uid, gid, mode, euid, egid, 4) {
            return NEG_EACCES;
        }
    }

    let offset = entry.offset;
    let max_bytes = (count as usize).min(64 * 1024);

    fn dirent_type_for_path(path: &str, is_dir: bool) -> u8 {
        if is_dir {
            DT_DIR
        } else {
            match path_node_nofollow(path) {
                Ok(PathNodeKind::Symlink(_)) => DT_LNK,
                Ok(PathNodeKind::Dir) => DT_DIR,
                _ => DT_REG,
            }
        }
    }

    // Collect directory entries: [(".", DT_DIR), ("..", DT_DIR), ...children...]
    let mut entries: alloc::vec::Vec<(alloc::string::String, u8)> = alloc::vec::Vec::new();
    entries.push((alloc::string::String::from("."), DT_DIR));
    entries.push((alloc::string::String::from(".."), DT_DIR));

    if crate::fs::procfs::is_dir(&dir_path) {
        match crate::fs::procfs::list_dir(&dir_path) {
            Some(children) => {
                for (name, is_dir) in children {
                    let child_path = if dir_path == "/" {
                        alloc::format!("/{name}")
                    } else {
                        alloc::format!("{dir_path}/{name}")
                    };
                    entries.push((name, dirent_type_for_path(&child_path, is_dir)));
                }
            }
            None => return NEG_ENOENT,
        }
    } else if let Some(rel) = tmpfs_relative_path(&dir_path) {
        let children = {
            let tmpfs = crate::fs::tmpfs::TMPFS.lock();
            match tmpfs.list_dir(rel) {
                Ok(children) => children,
                Err(_) => return NEG_ENOENT,
            }
        };
        for (name, is_dir) in children {
            let child_path = if dir_path == "/" {
                alloc::format!("/{name}")
            } else {
                alloc::format!("{dir_path}/{name}")
            };
            entries.push((name, dirent_type_for_path(&child_path, is_dir)));
        }
    } else if dir_path == "/" {
        // Root directory: merge ext2 root + ramdisk overlays + virtual mounts.
        // Start with ext2 root entries if mounted.
        let mut seen = alloc::collections::BTreeSet::new();
        if crate::fs::ext2::is_mounted() {
            let children = {
                let vol = crate::fs::ext2::EXT2_VOLUME.lock();
                vol.as_ref().and_then(|vol| vol.list_dir("/").ok())
            };
            if let Some(children) = children {
                for (name, is_dir) in children {
                    seen.insert(name.clone());
                    let child_path = alloc::format!("/{name}");
                    entries.push((name, dirent_type_for_path(&child_path, is_dir)));
                }
            }
        }
        // Overlay ramdisk top-level dirs (/bin, /sbin, /etc).
        if let Some(ramdisk_children) = crate::fs::ramdisk::ramdisk_list_dir("/") {
            for (name, is_dir) in ramdisk_children {
                if !seen.contains(&name) {
                    seen.insert(name.clone());
                    entries.push((name, if is_dir { DT_DIR } else { DT_REG }));
                }
            }
        }
        // Add virtual mount points.
        if !seen.contains("tmp") {
            entries.push((alloc::string::String::from("tmp"), DT_DIR));
        }
        if !seen.contains("proc") {
            entries.push((alloc::string::String::from("proc"), DT_DIR));
        }
        if !seen.contains("dev") {
            entries.push((alloc::string::String::from("dev"), DT_DIR));
        }
        if crate::fs::fat32::is_mounted() && !seen.contains("data") {
            entries.push((alloc::string::String::from("data"), DT_DIR));
        }
    } else if crate::fs::ext2::is_mounted() {
        // ext2 subdirectory listing (e.g. /home, /etc).
        if let Some(rel) = ext2_root_path(&dir_path) {
            // Merge entries from both ramdisk and ext2 for overlaid dirs.
            let mut seen = alloc::collections::BTreeSet::new();
            if let Some(children) = crate::fs::ramdisk::ramdisk_list_dir(&dir_path) {
                for (name, is_dir) in children {
                    seen.insert(name.clone());
                    entries.push((name, if is_dir { DT_DIR } else { DT_REG }));
                }
            }
            let children = {
                let vol = crate::fs::ext2::EXT2_VOLUME.lock();
                vol.as_ref().and_then(|vol| vol.list_dir(rel).ok())
            };
            if let Some(children) = children {
                for (name, is_dir) in children {
                    if !seen.contains(&name) {
                        let child_path = if dir_path == "/" {
                            alloc::format!("/{name}")
                        } else {
                            alloc::format!("{dir_path}/{name}")
                        };
                        entries.push((name, dirent_type_for_path(&child_path, is_dir)));
                    }
                }
            }
        }
    } else if let Some(rel) = fat32_relative_path(&dir_path) {
        // Legacy: /data directory listing for FAT32 fallback.
        if crate::fs::fat32::is_mounted() {
            let vol = crate::fs::fat32::FAT32_VOLUME.lock();
            if let Some(vol) = vol.as_ref() {
                let dir_cluster = if rel.is_empty() {
                    vol.bpb.root_cluster
                } else {
                    match vol.lookup(rel) {
                        Ok(e) if e.is_dir() => e.start_cluster(),
                        _ => return NEG_ENOENT,
                    }
                };
                match vol.list_dir(dir_cluster) {
                    Ok(children) => {
                        for (name, is_dir) in children {
                            entries.push((name, if is_dir { DT_DIR } else { DT_REG }));
                        }
                    }
                    Err(_) => return NEG_EIO,
                }
            }
        }
    } else {
        // Ramdisk directory listing.
        if let Some(children) = crate::fs::ramdisk::ramdisk_list_dir(&dir_path) {
            for (name, is_dir) in children {
                entries.push((name, if is_dir { DT_DIR } else { DT_REG }));
            }
        }
    }

    if offset >= entries.len() {
        return 0; // end of directory
    }

    // Serialize into a kernel buffer, then copy to userspace.
    let mut out: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    let mut idx = offset;

    while idx < entries.len() {
        let (ref name, d_type) = entries[idx];
        let name_bytes = name.as_bytes();
        // reclen = 19 (fixed fields) + name_len + 1 (null), rounded up to 8
        let reclen = (19 + name_bytes.len() + 1 + 7) & !7;

        if out.len() + reclen > max_bytes {
            if out.is_empty() {
                // Even one entry doesn't fit — EINVAL.
                return NEG_EINVAL;
            }
            break;
        }

        let start = out.len();
        out.resize(start + reclen, 0); // zero-pad

        let d_ino: u64 = (idx + 1) as u64;
        let d_off: i64 = (idx + 1) as i64;
        out[start..start + 8].copy_from_slice(&d_ino.to_ne_bytes());
        out[start + 8..start + 16].copy_from_slice(&d_off.to_ne_bytes());
        out[start + 16..start + 18].copy_from_slice(&(reclen as u16).to_ne_bytes());
        out[start + 18] = d_type;
        out[start + 19..start + 19 + name_bytes.len()].copy_from_slice(name_bytes);
        // null terminator and padding are already zero from resize

        idx += 1;
    }

    if out.is_empty() {
        return 0;
    }

    if crate::mm::user_mem::copy_to_user(buf_ptr, &out).is_err() {
        return NEG_EFAULT;
    }

    // Update the fd offset so the next call resumes.
    with_current_fd_mut(fd_idx, |slot| {
        if let Some(e) = slot {
            e.offset = idx;
        }
    });

    out.len() as u64
}

fn sys_umask(mask: u64) -> u64 {
    let new_mask = (mask as u16) & 0o777;
    let pid = crate::process::current_pid();
    let mut table = crate::process::PROCESS_TABLE.lock();
    let Some(proc) = table.find_mut(pid) else {
        return 0o022;
    };
    let old = proc.umask;
    proc.umask = new_mask;
    old as u64
}

// ---------------------------------------------------------------------------
// arch_prctl(code, addr) — syscall 158 (musl TLS initialization)
// ---------------------------------------------------------------------------

/// Handles `ARCH_SET_FS` (0x1002) which musl uses to set the FS.base MSR for
/// thread-local storage.  Other sub-commands return -EINVAL.
fn sys_linux_arch_prctl(code: u64, addr: u64) -> u64 {
    const ARCH_SET_FS: u64 = 0x1002;
    match code {
        ARCH_SET_FS => {
            let vaddr = match x86_64::VirtAddr::try_new(addr) {
                Ok(v) => v,
                Err(_) => return NEG_EINVAL,
            };
            x86_64::registers::model_specific::FsBase::write(vaddr);
            // Save FS.base to process table for context-switch restore.
            let pid = crate::process::current_pid();
            let mut table = crate::process::PROCESS_TABLE.lock();
            if let Some(proc) = table.find_mut(pid) {
                proc.fs_base = addr;
            }
            0
        }
        _ => NEG_EINVAL,
    }
}

// ---------------------------------------------------------------------------
// set_tid_address(tidptr) — syscall 218 (musl TLS initialization)
// ---------------------------------------------------------------------------

/// `set_tid_address(tidptr)` — store the `clear_child_tid` pointer for the
/// calling thread and return the caller's TID.
///
/// musl calls this during `__init_tls` to record the address that the kernel
/// should clear (and futex-wake) when the thread exits.
fn sys_linux_set_tid_address(tidptr: u64) -> u64 {
    let pid = crate::process::current_pid();

    {
        let mut table = crate::process::PROCESS_TABLE.lock();
        if let Some(proc) = table.find_mut(pid) {
            proc.clear_child_tid = tidptr;
            proc.tid as u64
        } else {
            pid as u64
        }
    }
}

// ===========================================================================
// Phase 21 — Ion Shell: syscall stubs for musl/nix runtime
// ===========================================================================

// ---------------------------------------------------------------------------
// access(path, mode) — syscall 21
// ---------------------------------------------------------------------------

/// Check if a path exists. Ignores the mode argument (no permission model).
fn sys_access(path_ptr: u64) -> u64 {
    let mut buf = [0u8; 512];
    let name = match read_user_cstr(path_ptr, &mut buf) {
        Some(n) => n,
        None => return NEG_EFAULT,
    };

    let cwd = current_cwd();
    let resolved = resolve_path(&cwd, name);

    // Phase 21: /dev/null always exists.
    // Phase 22: /dev/ptmx and /dev/pts/* always exist.
    if resolved == "/dev/null"
        || resolved == "/dev/zero"
        || resolved == "/dev/urandom"
        || resolved == "/dev/random"
        || resolved == "/dev/full"
        || resolved == "/dev/ptmx"
        || resolved.starts_with("/dev/pts/")
    {
        return 0;
    }

    // Check ramdisk.
    if crate::fs::ramdisk::ramdisk_lookup(&resolved).is_some() {
        return 0;
    }
    // Check tmpfs.
    if let Some(rel) = tmpfs_relative_path(&resolved) {
        if rel.is_empty() {
            return 0; // /tmp itself
        }
        let tmpfs = crate::fs::tmpfs::TMPFS.lock();
        if tmpfs.stat(rel).is_ok() {
            return 0;
        }
    }

    // Phase 31: check ext2 root filesystem.
    if crate::fs::ext2::is_mounted() {
        let vol = crate::fs::ext2::EXT2_VOLUME.lock();
        if let Some(vol) = vol.as_ref() {
            let rel = resolved.trim_start_matches('/');
            if vol.resolve_path(rel).is_ok() {
                return 0;
            }
        }
    }

    // Phase 31: check FAT32 (/data mount and /usr paths mapped onto it).
    {
        let fat_rel = if let Some(stripped) = resolved.strip_prefix("/data/") {
            Some(stripped)
        } else if resolved.starts_with("/usr/") {
            Some(resolved.trim_start_matches('/'))
        } else {
            None
        };
        if let Some(rel) = fat_rel {
            let vol = crate::fs::fat32::FAT32_VOLUME.lock();
            if let Some(vol) = vol.as_ref()
                && vol.lookup(rel).is_ok()
            {
                return 0;
            }
        }
    }

    NEG_ENOENT
}

// ---------------------------------------------------------------------------
// clone(flags, ...) — syscall 56
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Clone flags (Phase 40)
// ---------------------------------------------------------------------------

const SIGCHLD: u64 = 17;
const CLONE_VM: u64 = 0x0000_0100;
#[allow(dead_code)]
const CLONE_FS: u64 = 0x0000_0200;
#[allow(dead_code)]
const CLONE_FILES: u64 = 0x0000_0400;
#[allow(dead_code)]
const CLONE_SIGHAND: u64 = 0x0000_0800;
const CLONE_THREAD: u64 = 0x0001_0000;
const CLONE_VFORK: u64 = 0x0000_4000;
const CLONE_SETTLS: u64 = 0x0008_0000;
const CLONE_PARENT_SETTID: u64 = 0x0010_0000;
const CLONE_CHILD_CLEARTID: u64 = 0x0020_0000;
const CLONE_CHILD_SETTID: u64 = 0x0100_0000;

/// Parse clone(2) flags and dispatch to the appropriate implementation.
///
/// Linux clone ABI: flags (rdi), child_stack (rsi), parent_tidptr (rdx),
/// child_tidptr (r10), tls (r8).
fn sys_clone(
    flags: u64,
    child_stack: u64,
    parent_tidptr: u64,
    child_tidptr: u64,
    tls: u64,
    user_rip: u64,
    user_rsp: u64,
) -> u64 {
    // CLONE_THREAD requires CLONE_VM — threads must share address space.
    if flags & CLONE_THREAD != 0 && flags & CLONE_VM == 0 {
        log::warn!("sys_clone: CLONE_THREAD without CLONE_VM");
        return NEG_EINVAL;
    }

    // Thread creation path (Phase 40).
    if flags & CLONE_THREAD != 0 {
        return sys_clone_thread(
            flags,
            child_stack,
            parent_tidptr,
            child_tidptr,
            tls,
            user_rip,
        );
    }

    // musl uses clone(SIGCHLD, NULL, ...) as a fork fallback.
    // Accept flags == SIGCHLD, flags == 0, or the CLONE_VM|CLONE_VFORK
    // combination used by musl's posix_spawn/system() — treat all as fork.
    let fork_flags =
        flags & !(CLONE_CHILD_SETTID | CLONE_CHILD_CLEARTID | CLONE_PARENT_SETTID | CLONE_SETTLS);
    if fork_flags == 0
        || fork_flags == SIGCHLD
        || fork_flags == (CLONE_VM | CLONE_VFORK | SIGCHLD)
        || fork_flags == (CLONE_VM | CLONE_VFORK)
    {
        sys_fork(user_rip, user_rsp)
    } else {
        log::warn!("sys_clone: unsupported flags {flags:#x}");
        NEG_ENOSYS
    }
}

// ---------------------------------------------------------------------------
// clone(CLONE_THREAD) — Phase 40, Track C
// ---------------------------------------------------------------------------

/// Create a new thread sharing the parent's address space.
///
/// The child thread:
/// - gets a new PID/TID but shares the parent's TGID
/// - shares the parent's page table (no CoW clone)
/// - shares the parent's fd table and signal actions via Arc
/// - gets its own kernel stack
/// - starts executing in userspace at `user_rip` with RSP = `child_stack`
fn sys_clone_thread(
    flags: u64,
    child_stack: u64,
    parent_tidptr: u64,
    child_tidptr: u64,
    tls: u64,
    user_rip: u64,
) -> u64 {
    use crate::process::{
        PROCESS_TABLE, Process, ProcessState, ThreadGroup, alloc_kernel_stack_pub,
    };
    use alloc::sync::Arc;

    let parent_pid = crate::process::current_pid();
    log::info!(
        "[p{}] clone_thread(flags={:#x}, child_stack={:#x})",
        parent_pid,
        flags,
        child_stack
    );

    if child_stack == 0 {
        log::warn!("sys_clone_thread: child_stack is NULL");
        return NEG_EINVAL;
    }

    // Gather parent state under lock.
    let parent_info = {
        let table = PROCESS_TABLE.lock();
        match table.find(parent_pid) {
            Some(p) => {
                let cr3 = match p.page_table_root {
                    Some(cr3) => cr3,
                    None => {
                        log::warn!("sys_clone_thread: parent has no page table");
                        return NEG_EINVAL;
                    }
                };
                Some((
                    cr3,
                    p.tgid,
                    p.ppid,
                    p.brk_current,
                    p.mmap_next,
                    p.pgid,
                    p.cwd.clone(),
                    p.blocked_signals,
                    p.signal_actions_snapshot(),
                    p.fs_base,
                    (p.uid, p.gid, p.euid, p.egid),
                    p.umask,
                    p.session_id,
                    p.controlling_tty.clone(),
                    p.mappings.clone(),
                    p.exec_path.clone(),
                    p.cmdline.clone(),
                    p.fd_table_snapshot(),
                    p.thread_group.clone(),
                    p.shared_fd_table.clone(),
                    p.shared_signal_actions.clone(),
                ))
            }
            None => None,
        }
    };

    let (
        parent_cr3,
        parent_tgid,
        parent_ppid,
        parent_brk,
        parent_mmap,
        parent_pgid,
        parent_cwd,
        parent_blocked_signals,
        parent_signal_actions,
        parent_fs_base,
        parent_ids,
        parent_umask,
        parent_session_id,
        parent_ctty,
        parent_mappings,
        parent_exec_path,
        parent_cmdline,
        parent_fds,
        parent_thread_group,
        parent_shared_fd,
        parent_shared_sig,
    ) = match parent_info {
        Some(info) => info,
        None => {
            log::warn!("sys_clone_thread: parent {} not found", parent_pid);
            return NEG_EINVAL;
        }
    };

    // Allocate child PID (also serves as TID).
    let child_pid = crate::process::alloc_pid_pub();
    let child_tgid = parent_tgid;

    // Create or join the ThreadGroup.
    let thread_group = match parent_thread_group {
        Some(tg) => {
            // Parent already in a thread group — add child.
            tg.members.lock().push(child_pid);
            tg
        }
        None => {
            // First thread creation — create a new group with parent as leader.
            let tg = Arc::new(ThreadGroup {
                leader_tid: parent_tgid,
                members: spin::Mutex::new(alloc::vec![parent_tgid, child_pid]),
            });
            // Set the parent's thread_group under lock.
            {
                let mut table = PROCESS_TABLE.lock();
                if let Some(p) = table.find_mut(parent_pid) {
                    p.thread_group = Some(tg.clone());
                }
            }
            tg
        }
    };

    // Share fd table only when CLONE_FILES is set; otherwise child gets a
    // private copy (shared_fd_table stays None).
    // Clone parent_fds before the potential move into an Arc so it remains
    // available for the non-shared path in the child Process builder.
    let child_fds_copy = parent_fds.clone();
    let shared_fd = if flags & CLONE_FILES != 0 {
        Some(match parent_shared_fd {
            Some(arc) => arc,
            None => {
                let arc = Arc::new(spin::Mutex::new(parent_fds));
                // Update parent to use shared fd table.
                {
                    let mut table = PROCESS_TABLE.lock();
                    if let Some(p) = table.find_mut(parent_pid) {
                        p.shared_fd_table = Some(arc.clone());
                    }
                }
                arc
            }
        })
    } else {
        None
    };

    // Share signal actions only when CLONE_SIGHAND is set; otherwise child
    // gets its own private copy.
    let shared_sig = if flags & CLONE_SIGHAND != 0 {
        Some(match parent_shared_sig {
            Some(arc) => arc,
            None => {
                let arc = Arc::new(spin::Mutex::new(parent_signal_actions));
                {
                    let mut table = PROCESS_TABLE.lock();
                    if let Some(p) = table.find_mut(parent_pid) {
                        p.shared_signal_actions = Some(arc.clone());
                    }
                }
                arc
            }
        })
    } else {
        None
    };

    // Allocate a NEW kernel stack for the child thread.
    let kstack_top = alloc_kernel_stack_pub();

    // Determine TLS: if CLONE_SETTLS, use the provided tls value.
    let child_fs_base = if flags & CLONE_SETTLS != 0 {
        tls
    } else {
        parent_fs_base
    };

    // Determine clear_child_tid.
    let child_clear_tid = if flags & CLONE_CHILD_CLEARTID != 0 {
        child_tidptr
    } else {
        0
    };

    // Build the child Process entry.
    let child_proc = Process {
        pid: child_pid,
        tid: child_pid,
        tgid: child_tgid,
        clear_child_tid: child_clear_tid,
        ppid: parent_ppid,
        state: ProcessState::Ready,
        page_table_root: Some(parent_cr3), // SHARED — same physical page table
        kernel_stack_top: kstack_top,
        entry_point: user_rip,
        user_stack_top: child_stack,
        exit_code: None,
        stop_signal: 0,
        stop_reported: false,
        brk_current: parent_brk,
        mmap_next: parent_mmap,
        pgid: parent_pgid,
        fd_table: {
            // When sharing via Arc, snapshot from the shared table;
            // otherwise use the private copy.
            if let Some(ref arc) = shared_fd {
                arc.lock().clone()
            } else {
                child_fds_copy
            }
        },
        pending_signals: 0,
        blocked_signals: parent_blocked_signals,
        signal_actions: {
            if let Some(ref arc) = shared_sig {
                *arc.lock()
            } else {
                parent_signal_actions
            }
        },
        alt_stack_base: 0,
        alt_stack_size: 0,
        alt_stack_flags: 0,
        cwd: parent_cwd,
        fs_base: child_fs_base,
        uid: parent_ids.0,
        gid: parent_ids.1,
        euid: parent_ids.2,
        egid: parent_ids.3,
        umask: parent_umask,
        session_id: parent_session_id,
        controlling_tty: parent_ctty,
        mappings: parent_mappings,
        exec_path: parent_exec_path,
        cmdline: parent_cmdline,
        start_ticks: crate::arch::x86_64::interrupts::tick_count(),
        thread_group: Some(thread_group),
        shared_fd_table: shared_fd,
        shared_signal_actions: shared_sig,
    };

    PROCESS_TABLE.lock().insert(child_proc);

    // CLONE_PARENT_SETTID: write child TID to parent_tidptr in userspace.
    if flags & CLONE_PARENT_SETTID != 0 && parent_tidptr != 0 {
        let tid_bytes = (child_pid as i32).to_ne_bytes();
        let _ = crate::mm::user_mem::copy_to_user(parent_tidptr, &tid_bytes);
    }

    // CLONE_CHILD_SETTID: write child TID to child_tidptr in userspace.
    // Since we share the address space, we can write it now from the parent context.
    if flags & CLONE_CHILD_SETTID != 0 && child_tidptr != 0 {
        let tid_bytes = (child_pid as i32).to_ne_bytes();
        let _ = crate::mm::user_mem::copy_to_user(child_tidptr, &tid_bytes);
    }

    crate::task::spawn_fork_task(
        crate::process::make_fork_ctx_for_thread(child_pid, user_rip, child_stack),
        "clone-thread",
    );

    log::info!("[p{}] clone_thread → child tid {}", parent_pid, child_pid);
    child_pid as u64
}

// ---------------------------------------------------------------------------
// fcntl(fd, cmd, arg) — syscall 72
// ---------------------------------------------------------------------------

/// Minimal fcntl: F_DUPFD, F_GETFD, F_SETFD, F_GETFL, F_SETFL.
fn sys_fcntl(fd: u64, cmd: u64, arg: u64) -> u64 {
    const F_DUPFD: u64 = 0;
    const F_GETFD: u64 = 1;
    const F_SETFD: u64 = 2;
    const F_GETFL: u64 = 3;
    const F_SETFL: u64 = 4;
    const F_DUPFD_CLOEXEC: u64 = 1030;

    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            // Find the next free fd >= arg, duplicate oldfd into it.
            let set_cloexec = cmd == F_DUPFD_CLOEXEC;
            let oldfd = fd as usize;
            let min_fd = arg as usize;
            if oldfd >= MAX_FDS {
                return NEG_EBADF;
            }
            if min_fd >= MAX_FDS {
                return NEG_EINVAL;
            }
            let mut entry = match current_fd_entry(oldfd) {
                Some(e) => e,
                None => return NEG_EBADF,
            };
            if set_cloexec {
                entry.cloexec = true;
            }
            // Remember backend info so we only bump refcount on successful alloc.
            let backend_clone = entry.backend.clone();
            match alloc_fd(min_fd, entry) {
                Some(new_fd) => {
                    // Increment refcount only after successful allocation.
                    match &backend_clone {
                        FdBackend::PipeRead { pipe_id } => {
                            crate::pipe::pipe_add_reader(*pipe_id);
                        }
                        FdBackend::PipeWrite { pipe_id } => {
                            crate::pipe::pipe_add_writer(*pipe_id);
                        }
                        FdBackend::PtyMaster { pty_id } => {
                            crate::pty::add_master_ref(*pty_id);
                        }
                        FdBackend::PtySlave { pty_id } => {
                            crate::pty::add_slave_ref(*pty_id);
                        }
                        FdBackend::Socket { handle } => {
                            crate::net::add_socket_ref(*handle);
                        }
                        FdBackend::UnixSocket { handle } => {
                            crate::net::unix::add_unix_socket_ref(*handle);
                        }
                        FdBackend::Epoll { instance_id } => {
                            epoll_add_ref(*instance_id);
                        }
                        _ => {}
                    }
                    new_fd as u64
                }
                None => NEG_EMFILE,
            }
        }
        F_GETFD => {
            // Return FD_CLOEXEC (1) if cloexec is set.
            match current_fd_entry(fd as usize) {
                Some(e) => {
                    if e.cloexec {
                        1
                    } else {
                        0
                    }
                }
                None => NEG_EBADF,
            }
        }
        F_SETFD => {
            // arg & 1 = FD_CLOEXEC
            let cloexec = arg & 1 != 0;
            with_current_fd_mut(fd as usize, |slot| {
                if let Some(e) = slot {
                    e.cloexec = cloexec;
                }
            });
            0
        }
        F_GETFL => {
            const O_NONBLOCK: u64 = 0x800;
            const O_RDONLY: u64 = 0;
            const O_WRONLY: u64 = 1;
            const O_RDWR: u64 = 2;
            match current_fd_entry(fd as usize) {
                Some(e) => {
                    let mut flags = match (e.readable, e.writable) {
                        (true, true) => O_RDWR,
                        (false, true) => O_WRONLY,
                        _ => O_RDONLY,
                    };
                    if e.nonblock {
                        flags |= O_NONBLOCK;
                    }
                    flags
                }
                None => NEG_EBADF,
            }
        }
        F_SETFL => {
            const O_NONBLOCK: u64 = 0x800;
            if current_fd_entry(fd as usize).is_none() {
                return NEG_EBADF;
            }
            let nonblock = arg & O_NONBLOCK != 0;
            with_current_fd_mut(fd as usize, |slot| {
                if let Some(e) = slot {
                    e.nonblock = nonblock;
                }
            });
            0
        }
        _ => NEG_EINVAL,
    }
}

// ---------------------------------------------------------------------------
// getrandom(buf, buflen, flags) — syscall 318
// ---------------------------------------------------------------------------

/// Fill user buffer with pseudo-random bytes seeded from the TSC.
fn sys_getrandom(buf_ptr: u64, buflen: u64, _flags: u64) -> u64 {
    let len = buflen as usize;
    if len == 0 {
        return 0;
    }
    // Cap at 256 bytes per call to avoid large kernel allocations.
    let actual = len.min(256);
    let mut out = [0u8; 256];

    let mut state = seed_pseudorandom_state();
    fill_pseudorandom_bytes(&mut state, &mut out[..actual]);

    if crate::mm::user_mem::copy_to_user(buf_ptr, &out[..actual]).is_err() {
        return NEG_EFAULT;
    }
    actual as u64
}

// ---------------------------------------------------------------------------
// gettimeofday(tv) — syscall 96
// ---------------------------------------------------------------------------

/// LAPIC ticks per second (~100 Hz timer = 10ms per tick).
pub(crate) const TICKS_PER_SEC: u64 = 100;

/// Return wall-clock time (CLOCK_REALTIME) as struct timeval.
fn sys_gettimeofday(tv_ptr: u64) -> u64 {
    if tv_ptr == 0 {
        return NEG_EFAULT;
    }
    let boot_epoch = crate::rtc::BOOT_EPOCH_SECS.load(core::sync::atomic::Ordering::Relaxed);
    let ticks = crate::arch::x86_64::interrupts::tick_count();
    let tv_sec = boot_epoch + ticks / TICKS_PER_SEC;
    let tv_usec = (ticks % TICKS_PER_SEC) * (1_000_000 / TICKS_PER_SEC);
    // struct timeval: tv_sec (i64) + tv_usec (i64) = 16 bytes
    let mut buf = [0u8; 16];
    buf[0..8].copy_from_slice(&(tv_sec as i64).to_ne_bytes());
    buf[8..16].copy_from_slice(&(tv_usec as i64).to_ne_bytes());
    if crate::mm::user_mem::copy_to_user(tv_ptr, &buf).is_err() {
        return NEG_EFAULT;
    }
    0
}

// ---------------------------------------------------------------------------
// clock_gettime(clk_id, tp) — syscall 228
// ---------------------------------------------------------------------------

/// Clock IDs (Linux ABI).
const CLOCK_REALTIME: u64 = 0;
const CLOCK_MONOTONIC: u64 = 1;
const CLOCK_MONOTONIC_RAW: u64 = 4;
const CLOCK_REALTIME_COARSE: u64 = 5;
const CLOCK_MONOTONIC_COARSE: u64 = 6;

/// Return time as struct timespec, dispatching on clock ID.
fn sys_clock_gettime(clk_id: u64, tp_ptr: u64) -> u64 {
    if tp_ptr == 0 {
        return NEG_EFAULT;
    }
    let ticks = crate::arch::x86_64::interrupts::tick_count();
    let (secs, nsecs) = match clk_id {
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE => {
            let boot_epoch =
                crate::rtc::BOOT_EPOCH_SECS.load(core::sync::atomic::Ordering::Relaxed);
            let s = boot_epoch + ticks / TICKS_PER_SEC;
            let ns = (ticks % TICKS_PER_SEC) * (1_000_000_000 / TICKS_PER_SEC);
            (s, ns)
        }
        CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW | CLOCK_MONOTONIC_COARSE => {
            let s = ticks / TICKS_PER_SEC;
            let ns = (ticks % TICKS_PER_SEC) * (1_000_000_000 / TICKS_PER_SEC);
            (s, ns)
        }
        _ => return NEG_EINVAL,
    };
    // struct timespec: tv_sec (i64) + tv_nsec (i64) = 16 bytes
    let mut buf = [0u8; 16];
    buf[0..8].copy_from_slice(&(secs as i64).to_ne_bytes());
    buf[8..16].copy_from_slice(&(nsecs as i64).to_ne_bytes());
    if crate::mm::user_mem::copy_to_user(tp_ptr, &buf).is_err() {
        return NEG_EFAULT;
    }
    0
}

// ---------------------------------------------------------------------------
// futex(uaddr, op, val, ...) — syscall 202
// ---------------------------------------------------------------------------

/// Futex wait/wake implementation for thread synchronization.
/// Supports WAIT, WAKE, WAIT_BITSET, WAKE_BITSET with real blocking queues.
///
/// Supports `FUTEX_WAIT`, `FUTEX_WAKE`, `FUTEX_WAIT_BITSET`, and
/// `FUTEX_WAKE_BITSET` operations with the `FUTEX_PRIVATE_FLAG`.
fn sys_futex(uaddr: u64, op: u64, val: u64, val3: u64) -> u64 {
    const FUTEX_WAIT: u64 = 0;
    const FUTEX_WAKE: u64 = 1;
    const FUTEX_WAIT_BITSET: u64 = 9;
    const FUTEX_WAKE_BITSET: u64 = 10;
    const FUTEX_PRIVATE_FLAG: u64 = 128;

    use crate::process::futex::{FUTEX_BITSET_MATCH_ANY, FUTEX_TABLE, FutexWaiter};

    let is_private = (op & FUTEX_PRIVATE_FLAG) != 0;
    let cmd = op & !(FUTEX_PRIVATE_FLAG);

    // Build the futex key: (page_table_root, uaddr).
    // Private futexes use 0 as root; shared futexes use the real CR3.
    let page_table_root = if is_private {
        0u64
    } else {
        let pid = crate::process::current_pid();
        match crate::process::PROCESS_TABLE
            .lock()
            .find(pid)
            .and_then(|p| p.page_table_root.map(|a| a.as_u64()))
        {
            Some(root) => root,
            None => return NEG_EINVAL, // non-private futex requires a page table root
        }
    };
    let key = (page_table_root, uaddr);

    match cmd {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            let bitset = if cmd == FUTEX_WAIT_BITSET {
                let bs = val3 as u32;
                if bs == 0 {
                    return NEG_EINVAL;
                }
                bs
            } else {
                FUTEX_BITSET_MATCH_ANY
            };

            // Atomically: check *uaddr == val and enqueue waiter under the
            // FUTEX_TABLE lock so no wake can be missed between the check
            // and the block.
            let tid = match crate::task::current_task_id() {
                Some(id) => id,
                None => return NEG_EAGAIN,
            };

            // Single-threaded fast path: if this process has no thread group,
            // blocking would deadlock because no other thread exists to wake
            // us. Clear the futex word to 0 (matching the pre-Phase 40 stub
            // behavior that musl's __lock relies on) and return immediately.
            let is_single_threaded = {
                let pid = crate::process::current_pid();
                let table = crate::process::PROCESS_TABLE.lock();
                table
                    .find(pid)
                    .map(|p| p.thread_group.is_none())
                    .unwrap_or(true)
            };
            if is_single_threaded {
                let mut cur = [0u8; 4];
                if crate::mm::user_mem::copy_from_user(&mut cur, uaddr).is_err() {
                    return NEG_EFAULT;
                }
                if u32::from_ne_bytes(cur) as u64 != val {
                    return NEG_EAGAIN;
                }
                let _ = crate::mm::user_mem::copy_to_user(uaddr, &0u32.to_ne_bytes());
                return 0;
            }

            let woken_flag = alloc::sync::Arc::new(core::sync::atomic::AtomicBool::new(false));

            {
                let mut table = FUTEX_TABLE.lock();

                // Read the futex word from userspace.
                let mut cur = [0u8; 4];
                if crate::mm::user_mem::copy_from_user(&mut cur, uaddr).is_err() {
                    return NEG_EFAULT;
                }
                let current_val = u32::from_ne_bytes(cur) as u64;
                if current_val != val {
                    return NEG_EAGAIN;
                }

                // Value matches — enqueue this thread as a waiter.
                table.entry(key).or_default().push(FutexWaiter {
                    tid,
                    bitset,
                    woken: alloc::sync::Arc::clone(&woken_flag),
                });
            }

            // Atomically check the woken flag and block under the scheduler
            // lock to avoid a missed-wakeup race where a waker sets the flag
            // and calls wake_task() between our check and block.
            crate::task::block_current_on_futex_unless_woken(&woken_flag);

            // Woken — return success.
            0
        }

        FUTEX_WAKE | FUTEX_WAKE_BITSET => {
            let bitset = if cmd == FUTEX_WAKE_BITSET {
                let bs = val3 as u32;
                if bs == 0 {
                    return NEG_EINVAL;
                }
                bs
            } else {
                FUTEX_BITSET_MATCH_ANY
            };

            let max_wake = val as usize;
            let mut woken_count = 0usize;

            let to_wake = {
                let mut table = FUTEX_TABLE.lock();
                let mut wake_list = alloc::vec::Vec::new();
                if let Some(waiters) = table.get_mut(&key) {
                    let mut i = 0;
                    while i < waiters.len() && woken_count < max_wake {
                        if (waiters[i].bitset & bitset) != 0 {
                            let w = waiters.remove(i);
                            // Set the woken flag *before* calling wake_task so the
                            // waiter can detect the wake even if it has not blocked yet.
                            w.woken.store(true, core::sync::atomic::Ordering::Release);
                            wake_list.push(w.tid);
                            woken_count += 1;
                            // Don't increment i — remove shifted elements down.
                        } else {
                            i += 1;
                        }
                    }
                    // Clean up empty entries.
                    if waiters.is_empty() {
                        table.remove(&key);
                    }
                }
                wake_list
            };

            // Wake the tasks outside the FUTEX_TABLE lock.
            // Only count tasks that were actually transitioned to Ready
            // (skip Dead or already-woken tasks).
            let mut actual_woken = 0usize;
            for tid in to_wake {
                if crate::task::wake_task(tid) {
                    actual_woken += 1;
                }
            }

            actual_woken as u64
        }

        _ => 0, // Unknown ops succeed silently (Linux compat).
    }
}

// ---------------------------------------------------------------------------
// Phase 23: Socket syscalls
// ---------------------------------------------------------------------------

/// Helper: read a SockaddrIn from userspace and return (ip, port).
fn sockaddr_from_user(addr_ptr: u64) -> Result<([u8; 4], u16), u64> {
    let mut buf = [0u8; 16]; // sizeof(sockaddr_in)
    if crate::mm::user_mem::copy_from_user(&mut buf, addr_ptr).is_err() {
        return Err(NEG_EFAULT);
    }
    let family = u16::from_ne_bytes([buf[0], buf[1]]);
    if family != 2 {
        // AF_INET
        return Err(NEG_EINVAL);
    }
    let port = u16::from_be_bytes([buf[2], buf[3]]);
    let ip = [buf[4], buf[5], buf[6], buf[7]];
    Ok((ip, port))
}

/// Helper: write a SockaddrIn to userspace.
fn sockaddr_to_user(addr_ptr: u64, ip: [u8; 4], port: u16) -> Result<(), u64> {
    if addr_ptr == 0 {
        return Ok(());
    }
    let mut buf = [0u8; 16];
    buf[0..2].copy_from_slice(&2u16.to_ne_bytes()); // AF_INET
    buf[2..4].copy_from_slice(&port.to_be_bytes());
    buf[4..8].copy_from_slice(&ip);
    if crate::mm::user_mem::copy_to_user(addr_ptr, &buf).is_err() {
        return Err(NEG_EFAULT);
    }
    Ok(())
}

/// Helper: look up socket handle from fd. Returns (handle, socket_kind, protocol).
fn socket_handle_from_fd(
    fd: u64,
) -> Result<
    (
        crate::net::SocketHandle,
        crate::net::SocketKind,
        crate::net::SocketProtocol,
    ),
    u64,
> {
    let fd_idx = fd as usize;
    if fd_idx >= MAX_FDS {
        return Err(NEG_EBADF);
    }
    let entry = current_fd_entry(fd_idx).ok_or(NEG_EBADF)?;
    match &entry.backend {
        FdBackend::Socket { handle } => {
            let h = *handle;
            let info = crate::net::with_socket(h, |s| (s.kind, s.protocol));
            match info {
                Some((kind, proto)) => Ok((h, kind, proto)),
                None => Err(NEG_EBADF),
            }
        }
        _ => Err(NEG_ENOTSOCK),
    }
}

const NEG_ENOTSOCK: u64 = (-88_i64) as u64;
const NEG_ENFILE: u64 = (-23_i64) as u64;
const NEG_EADDRINUSE: u64 = (-98_i64) as u64;
const NEG_ENOTCONN: u64 = (-107_i64) as u64;
const NEG_ECONNREFUSED: u64 = (-111_i64) as u64;
const NEG_ETIMEDOUT: u64 = (-110_i64) as u64;
const NEG_EOPNOTSUPP: u64 = (-95_i64) as u64;
const NEG_ENOPROTOOPT: u64 = (-92_i64) as u64;
const NEG_EAFNOSUPPORT: u64 = (-97_i64) as u64;
const NEG_EISCONN: u64 = (-106_i64) as u64;
const NEG_EALREADY: u64 = (-114_i64) as u64;

// ===========================================================================
// Phase 39: Unix domain socket syscall helpers
// ===========================================================================

/// Create an AF_UNIX socket.
fn sys_socket_unix(socktype: u64) -> u64 {
    const SOCK_NONBLOCK: u64 = 0x800;
    const SOCK_CLOEXEC: u64 = 0x80000;
    let flags = socktype & (SOCK_CLOEXEC | SOCK_NONBLOCK);
    let raw_type = socktype & !(SOCK_CLOEXEC | SOCK_NONBLOCK);
    let unix_type = match raw_type {
        1 | 5 => crate::net::unix::UnixSocketType::Stream, // SOCK_STREAM or SOCK_SEQPACKET
        2 => crate::net::unix::UnixSocketType::Datagram,
        _ => return NEG_EINVAL,
    };
    let handle = match crate::net::unix::alloc_unix_socket(unix_type) {
        Some(h) => h,
        None => return NEG_ENFILE,
    };
    let entry = FdEntry {
        backend: FdBackend::UnixSocket { handle },
        offset: 0,
        readable: true,
        writable: true,
        cloexec: flags & SOCK_CLOEXEC != 0,
        nonblock: flags & SOCK_NONBLOCK != 0,
    };
    match alloc_fd(0, entry) {
        Some(fd) => fd as u64,
        None => {
            crate::net::unix::free_unix_socket(handle);
            NEG_EMFILE
        }
    }
}

/// socketpair(domain, type, protocol, sv) — syscall 53
fn sys_socketpair(domain: u64, socktype: u64, _protocol: u64, sv_ptr: u64) -> u64 {
    const AF_UNIX: u64 = 1;
    const SOCK_NONBLOCK: u64 = 0x800;
    const SOCK_CLOEXEC: u64 = 0x80000;

    if domain != AF_UNIX {
        // Fall back to pipe-based socketpair for non-AF_UNIX.
        let cloexec = socktype & SOCK_CLOEXEC != 0;
        return sys_pipe_with_flags(sv_ptr, cloexec);
    }

    let flags = socktype & (SOCK_CLOEXEC | SOCK_NONBLOCK);
    let raw_type = socktype & !(SOCK_CLOEXEC | SOCK_NONBLOCK);
    let unix_type = match raw_type {
        1 | 5 => crate::net::unix::UnixSocketType::Stream, // SOCK_STREAM or SOCK_SEQPACKET
        2 => crate::net::unix::UnixSocketType::Datagram,
        _ => return NEG_EINVAL,
    };

    let h1 = match crate::net::unix::alloc_unix_socket(unix_type) {
        Some(h) => h,
        None => return NEG_ENFILE,
    };
    let h2 = match crate::net::unix::alloc_unix_socket(unix_type) {
        Some(h) => h,
        None => {
            crate::net::unix::free_unix_socket(h1);
            return NEG_ENFILE;
        }
    };

    // Peer them together and mark as connected.
    crate::net::unix::with_unix_socket_mut(h1, |s| {
        s.peer = Some(h2);
        s.state = crate::net::unix::UnixSocketState::Connected;
    });
    crate::net::unix::with_unix_socket_mut(h2, |s| {
        s.peer = Some(h1);
        s.state = crate::net::unix::UnixSocketState::Connected;
    });

    let cloexec = flags & SOCK_CLOEXEC != 0;
    let nonblock = flags & SOCK_NONBLOCK != 0;

    let fd1 = match alloc_fd(
        0,
        FdEntry {
            backend: FdBackend::UnixSocket { handle: h1 },
            offset: 0,
            readable: true,
            writable: true,
            cloexec,
            nonblock,
        },
    ) {
        Some(fd) => fd,
        None => {
            crate::net::unix::free_unix_socket(h1);
            crate::net::unix::free_unix_socket(h2);
            return NEG_EMFILE;
        }
    };
    let fd2 = match alloc_fd(
        0,
        FdEntry {
            backend: FdBackend::UnixSocket { handle: h2 },
            offset: 0,
            readable: true,
            writable: true,
            cloexec,
            nonblock,
        },
    ) {
        Some(fd) => fd,
        None => {
            // Close fd1
            with_current_fd_mut(fd1, |slot| *slot = None);
            crate::net::unix::free_unix_socket(h1);
            crate::net::unix::free_unix_socket(h2);
            return NEG_EMFILE;
        }
    };

    // Write [fd1, fd2] to userspace sv[2] array.
    let mut sv_bytes = [0u8; 8];
    sv_bytes[..4].copy_from_slice(&(fd1 as i32).to_ne_bytes());
    sv_bytes[4..].copy_from_slice(&(fd2 as i32).to_ne_bytes());
    if crate::mm::user_mem::copy_to_user(sv_ptr, &sv_bytes).is_err() {
        // Clean up on fault.
        with_current_fd_mut(fd1, |slot| *slot = None);
        with_current_fd_mut(fd2, |slot| *slot = None);
        crate::net::unix::free_unix_socket(h1);
        crate::net::unix::free_unix_socket(h2);
        return NEG_EFAULT;
    }
    0
}

/// Parse a sockaddr_un from userspace. Returns the path string.
fn sockaddr_un_from_user(addr_ptr: u64, addr_len: u64) -> Result<alloc::string::String, u64> {
    if addr_len < 3 {
        return Err(NEG_EINVAL); // Must have at least family + 1 byte of path
    }
    let len = (addr_len as usize).min(110); // sun_family(2) + path(up to 108)
    let mut buf = [0u8; 110];
    if crate::mm::user_mem::copy_from_user(&mut buf[..len], addr_ptr).is_err() {
        return Err(NEG_EFAULT);
    }
    // Validate sun_family == AF_UNIX (1)
    let family = u16::from_ne_bytes([buf[0], buf[1]]);
    if family != 1 {
        return Err(NEG_EAFNOSUPPORT);
    }
    // Extract NUL-terminated path from bytes [2..len].
    let path_bytes = &buf[2..len];
    let path_len = path_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(path_bytes.len());
    if path_len == 0 {
        return Err(NEG_EINVAL);
    }
    if path_len > 107 {
        return Err(NEG_EINVAL);
    }
    match core::str::from_utf8(&path_bytes[..path_len]) {
        Ok(s) => Ok(alloc::string::String::from(s)),
        Err(_) => Err(NEG_EINVAL),
    }
}

/// Helper: get Unix socket handle from FD, or ENOTSOCK.
fn unix_socket_handle_from_fd(fd: u64) -> Result<(usize, FdEntry), u64> {
    let fd_idx = fd as usize;
    if fd_idx >= MAX_FDS {
        return Err(NEG_EBADF);
    }
    let entry = current_fd_entry(fd_idx).ok_or(NEG_EBADF)?;
    match &entry.backend {
        FdBackend::UnixSocket { handle } => Ok((*handle, entry)),
        _ => Err(NEG_ENOTSOCK),
    }
}

/// bind() for Unix sockets.
fn sys_bind_unix(fd: u64, addr_ptr: u64, addr_len: u64) -> u64 {
    let (handle, _entry) = match unix_socket_handle_from_fd(fd) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = match sockaddr_un_from_user(addr_ptr, addr_len) {
        Ok(p) => p,
        Err(e) => return e,
    };

    // Check socket is unbound.
    let state = crate::net::unix::with_unix_socket(handle, |s| s.state);
    if state != Some(crate::net::unix::UnixSocketState::Unbound) {
        return NEG_EINVAL;
    }

    // Register the path in the path map.
    if crate::net::unix::bind_path(&path, handle).is_err() {
        return NEG_EADDRINUSE;
    }

    // Create a socket node in tmpfs if the path is under /tmp.
    if let Some(rel) = path.strip_prefix("/tmp/") {
        let pid = crate::process::current_pid();
        let (uid, gid, umask) = {
            let table = crate::process::PROCESS_TABLE.lock();
            match table.find(pid) {
                Some(p) => (p.uid, p.gid, p.umask),
                None => (0, 0, 0o022),
            }
        };
        let mode = 0o777 & !umask;
        let mut tmpfs = crate::fs::tmpfs::TMPFS.lock();
        match tmpfs.create_file_with_meta(rel, uid, gid, mode) {
            Ok(_) => {}
            Err(crate::fs::tmpfs::TmpfsError::AlreadyExists) => {
                drop(tmpfs);
                crate::net::unix::unbind_path(&path);
                return NEG_EADDRINUSE;
            }
            Err(_) => {
                drop(tmpfs);
                crate::net::unix::unbind_path(&path);
                return NEG_EIO;
            }
        }
    }

    // Update socket state.
    crate::net::unix::with_unix_socket_mut(handle, |s| {
        s.path = Some(path);
        s.state = crate::net::unix::UnixSocketState::Bound;
    });
    0
}

/// connect() for Unix stream sockets.
fn sys_connect_unix(fd: u64, addr_ptr: u64, addr_len: u64) -> u64 {
    let (handle, entry) = match unix_socket_handle_from_fd(fd) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = match sockaddr_un_from_user(addr_ptr, addr_len) {
        Ok(p) => p,
        Err(e) => return e,
    };

    let sock_type = match crate::net::unix::with_unix_socket(handle, |s| s.socket_type) {
        Some(t) => t,
        None => return NEG_EBADF,
    };

    // Look up the target socket.
    let target_handle = match crate::net::unix::lookup_path(&path) {
        Some(h) => h,
        None => return NEG_ECONNREFUSED,
    };

    match sock_type {
        crate::net::unix::UnixSocketType::Stream => {
            // Guard: reject if already connected or already pending connect.
            let cur_state = crate::net::unix::with_unix_socket(handle, |s| s.state);
            match cur_state {
                Some(crate::net::unix::UnixSocketState::Connected) => return NEG_EISCONN,
                Some(crate::net::unix::UnixSocketState::Connecting) => return NEG_EALREADY,
                _ => {}
            }

            // Verify target is listening.
            let is_listening = crate::net::unix::with_unix_socket(target_handle, |s| {
                matches!(s.state, crate::net::unix::UnixSocketState::Listening)
            });
            if is_listening != Some(true) {
                return NEG_ECONNREFUSED;
            }

            // Check backlog space.
            let backlog_full = crate::net::unix::with_unix_socket(target_handle, |s| {
                s.backlog.len() >= s.backlog_limit
            });
            if backlog_full == Some(true) {
                return NEG_ECONNREFUSED;
            }

            // Increment refcount before enqueuing to prevent use-after-free
            // if the client FD is closed before accept() processes the entry.
            crate::net::unix::add_unix_socket_ref(handle);

            // Mark as Connecting to prevent duplicate backlog entries.
            crate::net::unix::with_unix_socket_mut(handle, |s| {
                s.state = crate::net::unix::UnixSocketState::Connecting;
            });

            // Add ourselves to the listener's backlog.
            crate::net::unix::with_unix_socket_mut(target_handle, |s| {
                s.backlog.push_back(handle);
            });
            // Wake the listener.
            crate::net::unix::wake_unix_socket(target_handle);

            // Block until accepted (state transitions to Connected) or return EAGAIN.
            let nonblock = entry.nonblock;
            let pid = crate::process::current_pid();
            let saved_user_rsp = crate::smp::per_core().syscall_user_rsp;
            loop {
                let state = crate::net::unix::with_unix_socket(handle, |s| s.state);
                if state == Some(crate::net::unix::UnixSocketState::Connected) {
                    return 0;
                }
                if nonblock || has_pending_signal() {
                    // Roll back: remove from backlog, reset state, release refcount.
                    crate::net::unix::with_unix_socket_mut(target_handle, |s| {
                        s.backlog.retain(|&h| h != handle);
                    });
                    crate::net::unix::with_unix_socket_mut(handle, |s| {
                        s.state = crate::net::unix::UnixSocketState::Unbound;
                    });
                    crate::net::unix::free_unix_socket(handle);
                    return if nonblock { NEG_EAGAIN } else { NEG_EINTR };
                }
                crate::net::unix::UNIX_SOCKET_WAITQUEUES[handle].sleep();
                restore_caller_context(pid, saved_user_rsp);
            }
        }
        crate::net::unix::UnixSocketType::Datagram => {
            // For datagram sockets, just set default destination.
            crate::net::unix::with_unix_socket_mut(handle, |s| {
                s.peer = Some(target_handle);
            });
            0
        }
    }
}

/// listen() for Unix stream sockets.
fn sys_listen_unix(handle: usize, backlog: u64) -> u64 {
    let info = crate::net::unix::with_unix_socket(handle, |s| (s.state, s.socket_type));
    match info {
        Some((
            crate::net::unix::UnixSocketState::Bound,
            crate::net::unix::UnixSocketType::Stream,
        )) => {}
        _ => return NEG_EINVAL,
    }
    let limit = (backlog as usize).clamp(1, 16);
    crate::net::unix::with_unix_socket_mut(handle, |s| {
        s.state = crate::net::unix::UnixSocketState::Listening;
        s.backlog_limit = limit;
    });
    0
}

/// accept() for Unix stream sockets.
fn sys_accept_unix(fd: u64, addr_ptr: u64, addr_len_ptr: u64, flags: u64) -> u64 {
    let (handle, entry) = match unix_socket_handle_from_fd(fd) {
        Ok(v) => v,
        Err(e) => return e,
    };

    // Validate this is a listening stream socket.
    let info = crate::net::unix::with_unix_socket(handle, |s| (s.socket_type, s.state));
    match info {
        Some((
            crate::net::unix::UnixSocketType::Stream,
            crate::net::unix::UnixSocketState::Listening,
        )) => {}
        _ => return NEG_EINVAL,
    }

    let nonblock = entry.nonblock;
    let pid = crate::process::current_pid();
    let saved_user_rsp = crate::smp::per_core().syscall_user_rsp;

    const SOCK_NONBLOCK: u64 = 0x800;
    const SOCK_CLOEXEC: u64 = 0x80000;

    loop {
        // Try to dequeue a pending connection.
        let client_handle =
            crate::net::unix::with_unix_socket_mut(handle, |s| s.backlog.pop_front());
        if let Some(Some(ch)) = client_handle {
            // Allocate a new server-side socket.
            let server_handle =
                match crate::net::unix::alloc_unix_socket(crate::net::unix::UnixSocketType::Stream)
                {
                    Some(h) => h,
                    None => {
                        // Release backlog refcount on failure.
                        crate::net::unix::free_unix_socket(ch);
                        return NEG_ENFILE;
                    }
                };

            // Peer the server socket with the client.
            crate::net::unix::with_unix_socket_mut(server_handle, |s| {
                s.peer = Some(ch);
                s.state = crate::net::unix::UnixSocketState::Connected;
            });
            crate::net::unix::with_unix_socket_mut(ch, |s| {
                s.peer = Some(server_handle);
                s.state = crate::net::unix::UnixSocketState::Connected;
            });
            // Wake the client (blocked in connect).
            crate::net::unix::wake_unix_socket(ch);

            // Release the backlog refcount now that peering is complete.
            crate::net::unix::free_unix_socket(ch);

            // Create FD for the accepted socket.
            let new_entry = FdEntry {
                backend: FdBackend::UnixSocket {
                    handle: server_handle,
                },
                offset: 0,
                readable: true,
                writable: true,
                cloexec: flags & SOCK_CLOEXEC != 0,
                nonblock: flags & SOCK_NONBLOCK != 0,
            };
            let new_fd = match alloc_fd(0, new_entry) {
                Some(fd) => fd,
                None => {
                    crate::net::unix::free_unix_socket(server_handle);
                    return NEG_EMFILE;
                }
            };

            // Write peer address if requested.
            if addr_ptr != 0 && addr_len_ptr != 0 {
                let peer_path =
                    crate::net::unix::with_unix_socket(ch, |s| s.path.clone()).flatten();
                let _ = sockaddr_un_to_user(addr_ptr, addr_len_ptr, peer_path.as_deref());
            }

            return new_fd as u64;
        }

        if nonblock {
            return NEG_EAGAIN;
        }
        if has_pending_signal() {
            return NEG_EINTR;
        }
        crate::net::unix::UNIX_SOCKET_WAITQUEUES[handle].sleep();
        restore_caller_context(pid, saved_user_rsp);
    }
}

/// Helper: write a sockaddr_un to userspace.
fn sockaddr_un_to_user(addr_ptr: u64, addr_len_ptr: u64, path: Option<&str>) -> Result<(), u64> {
    // Read the caller-provided buffer size.
    let mut caller_len_bytes = [0u8; 4];
    if crate::mm::user_mem::copy_from_user(&mut caller_len_bytes, addr_len_ptr).is_err() {
        return Err(NEG_EFAULT);
    }
    let caller_len = u32::from_ne_bytes(caller_len_bytes) as usize;

    let mut buf = [0u8; 110];
    buf[0] = 1; // AF_UNIX
    buf[1] = 0;
    let total_len = if let Some(p) = path {
        let bytes = p.as_bytes();
        let copy_len = bytes.len().min(107);
        buf[2..2 + copy_len].copy_from_slice(&bytes[..copy_len]);
        2 + copy_len + 1 // family + path + NUL
    } else {
        2 // just the family
    };
    // Only write up to the caller's buffer size.
    let write_len = total_len.min(caller_len);
    if write_len > 0 && crate::mm::user_mem::copy_to_user(addr_ptr, &buf[..write_len]).is_err() {
        return Err(NEG_EFAULT);
    }
    // Write back the actual address length.
    let len_val = (total_len as u32).to_ne_bytes();
    if crate::mm::user_mem::copy_to_user(addr_len_ptr, &len_val).is_err() {
        return Err(NEG_EFAULT);
    }
    Ok(())
}

/// shutdown() for Unix sockets.
fn sys_shutdown_unix(handle: usize, how: u64) -> u64 {
    let peer = crate::net::unix::with_unix_socket_mut(handle, |s| {
        match how {
            0 => s.shut_rd = true, // SHUT_RD
            1 => s.shut_wr = true, // SHUT_WR
            2 => {
                s.shut_rd = true;
                s.shut_wr = true;
            } // SHUT_RDWR
            _ => return None,
        }
        s.peer
    });
    match peer {
        Some(Some(p)) => {
            crate::net::unix::wake_unix_socket(p);
            crate::net::unix::wake_unix_socket(handle);
            0
        }
        Some(None) => {
            crate::net::unix::wake_unix_socket(handle);
            0
        }
        None => NEG_EINVAL,
    }
}

/// sendto() for Unix datagram sockets.
fn sys_sendto_unix(
    handle: usize,
    buf_ptr: u64,
    len: u64,
    nonblock: bool,
    addr_ptr: u64,
    addr_len: u64,
) -> u64 {
    let capped = (len as usize).min(4096);
    let mut data = alloc::vec![0u8; capped];
    if crate::mm::user_mem::copy_from_user(&mut data, buf_ptr).is_err() {
        return NEG_EFAULT;
    }

    let sender_path = crate::net::unix::with_unix_socket(handle, |s| s.path.clone()).flatten();

    // Determine target: explicit addr or connected peer.
    let target = if addr_ptr != 0 && addr_len >= 3 {
        let path = match sockaddr_un_from_user(addr_ptr, addr_len) {
            Ok(p) => p,
            Err(e) => return e,
        };
        match crate::net::unix::lookup_path(&path) {
            Some(h) => h,
            None => return NEG_ECONNREFUSED,
        }
    } else {
        match crate::net::unix::with_unix_socket(handle, |s| s.peer).flatten() {
            Some(p) => p,
            None => return NEG_ENOTCONN,
        }
    };

    let pid = crate::process::current_pid();
    let saved_user_rsp = crate::smp::per_core().syscall_user_rsp;
    loop {
        match crate::net::unix::unix_dgram_send(sender_path.clone(), target, &data) {
            Ok(n) => {
                crate::net::unix::wake_unix_socket(target);
                return n as u64;
            }
            Err(-11) => {
                // EAGAIN — queue full, block or return.
                if nonblock {
                    return NEG_EAGAIN;
                }
                if has_pending_signal() {
                    return NEG_EINTR;
                }
                crate::net::unix::UNIX_SOCKET_WAITQUEUES[target].sleep();
                restore_caller_context(pid, saved_user_rsp);
            }
            Err(e) => return e as u64, // ECONNREFUSED, etc.
        }
    }
}

/// recvfrom() for Unix datagram sockets.
fn sys_recvfrom_unix(
    handle: usize,
    buf_ptr: u64,
    count: u64,
    nonblock: bool,
    addr_ptr: u64,
    addr_len_ptr: u64,
) -> u64 {
    let pid = crate::process::current_pid();
    let saved_user_rsp = crate::smp::per_core().syscall_user_rsp;
    let capped = (count as usize).min(4096);
    let mut tmp = alloc::vec![0u8; capped];
    loop {
        match crate::net::unix::unix_dgram_recv(handle, &mut tmp) {
            Ok((n, sender_path)) => {
                if crate::mm::user_mem::copy_to_user(buf_ptr, &tmp[..n]).is_err() {
                    return NEG_EFAULT;
                }
                if addr_ptr != 0 && addr_len_ptr != 0 {
                    let _ = sockaddr_un_to_user(addr_ptr, addr_len_ptr, sender_path.as_deref());
                }
                return n as u64;
            }
            Err(_) => {
                if nonblock {
                    return NEG_EAGAIN;
                }
                if has_pending_signal() {
                    return NEG_EINTR;
                }
                crate::net::unix::UNIX_SOCKET_WAITQUEUES[handle].sleep();
                restore_caller_context(pid, saved_user_rsp);
            }
        }
    }
}

/// socket(domain, type, protocol) — syscall 41
fn sys_socket(domain: u64, socktype: u64, protocol: u64) -> u64 {
    use crate::net::{SocketKind, SocketProtocol};
    const AF_UNIX: u64 = 1;
    const AF_INET: u64 = 2;

    if domain == AF_UNIX {
        return sys_socket_unix(socktype);
    }
    if domain != AF_INET {
        return NEG_EAFNOSUPPORT;
    }
    const SOCK_NONBLOCK: u64 = 0x800;
    const SOCK_CLOEXEC: u64 = 0x80000;
    let sock_flags = socktype & (SOCK_CLOEXEC | SOCK_NONBLOCK);
    let socktype_raw = socktype & !(SOCK_CLOEXEC | SOCK_NONBLOCK);
    let (kind, proto) = match socktype_raw {
        1 => (SocketKind::Stream, SocketProtocol::Tcp), // SOCK_STREAM
        2 => {
            // SOCK_DGRAM — protocol determines UDP vs ICMP
            if protocol == 1 {
                (SocketKind::Dgram, SocketProtocol::Icmp) // IPPROTO_ICMP
            } else {
                (SocketKind::Dgram, SocketProtocol::Udp) // default to UDP
            }
        }
        _ => return NEG_EINVAL,
    };
    let handle = match crate::net::alloc_socket(kind, proto) {
        Some(h) => h,
        None => return NEG_ENFILE,
    };
    let entry = FdEntry {
        backend: FdBackend::Socket { handle },
        offset: 0,
        readable: true,
        writable: true,
        cloexec: sock_flags & SOCK_CLOEXEC != 0,
        nonblock: sock_flags & SOCK_NONBLOCK != 0,
    };
    match alloc_fd(0, entry) {
        Some(fd) => fd as u64,
        None => {
            crate::net::free_socket(handle);
            NEG_EMFILE
        }
    }
}

/// bind(fd, addr, addrlen) — syscall 49
fn sys_bind(fd: u64, addr_ptr: u64, addr_len: u64) -> u64 {
    // Check for Unix socket first.
    if let Ok((_, _)) = unix_socket_handle_from_fd(fd) {
        return sys_bind_unix(fd, addr_ptr, addr_len);
    }
    let (handle, kind, proto) = match socket_handle_from_fd(fd) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if addr_len < 16 {
        return NEG_EINVAL;
    }
    let (ip, port) = match sockaddr_from_user(addr_ptr) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let local_ip = if ip == [0, 0, 0, 0] {
        crate::net::config::our_ip()
    } else {
        ip
    };

    match proto {
        crate::net::SocketProtocol::Udp => {
            if !crate::net::udp::bind(port) {
                return NEG_EADDRINUSE;
            }
            crate::net::with_socket_mut(handle, |s| {
                s.local_addr = local_ip;
                s.local_port = port;
                s.udp_bound = true;
                s.state = crate::net::SocketState::Bound;
            });
        }
        crate::net::SocketProtocol::Tcp => {
            crate::net::with_socket_mut(handle, |s| {
                s.local_addr = local_ip;
                s.local_port = port;
                s.state = crate::net::SocketState::Bound;
            });
        }
        crate::net::SocketProtocol::Icmp => {
            crate::net::with_socket_mut(handle, |s| {
                s.local_addr = local_ip;
                s.state = crate::net::SocketState::Bound;
            });
        }
    }
    let _ = kind;
    0
}

/// connect(fd, addr, addrlen) — syscall 42
fn sys_connect(fd: u64, addr_ptr: u64, addr_len: u64) -> u64 {
    if let Ok((_, _)) = unix_socket_handle_from_fd(fd) {
        return sys_connect_unix(fd, addr_ptr, addr_len);
    }
    let (handle, _kind, proto) = match socket_handle_from_fd(fd) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if addr_len < 16 {
        return NEG_EINVAL;
    }
    let (ip, port) = match sockaddr_from_user(addr_ptr) {
        Ok(v) => v,
        Err(e) => return e,
    };

    match proto {
        crate::net::SocketProtocol::Tcp => {
            // Allocate a TCP connection slot
            let local_port = crate::net::with_socket(handle, |s| {
                if s.local_port == 0 {
                    // Auto-assign ephemeral port
                    crate::arch::x86_64::interrupts::tick_count() as u16 | 0x8000
                } else {
                    s.local_port
                }
            })
            .unwrap_or(0x8000);

            let tcp_idx = match crate::net::tcp::create(local_port) {
                Some(idx) => idx,
                None => return NEG_EAGAIN, // no TCP slots
            };
            crate::net::tcp::connect(tcp_idx, ip, port);
            crate::net::with_socket_mut(handle, |s| {
                s.tcp_slot = Some(tcp_idx);
                s.remote_addr = ip;
                s.remote_port = port;
                s.local_port = local_port;
                s.local_addr = crate::net::config::our_ip();
            });

            // Block until connected or error
            let pid = crate::process::current_pid();
            let saved_user_rsp = per_core_syscall_user_rsp();
            let start_tick = crate::arch::x86_64::interrupts::tick_count();
            loop {
                let state = crate::net::tcp::state(tcp_idx);
                match state {
                    crate::net::tcp::TcpState::Established => {
                        crate::net::with_socket_mut(handle, |s| {
                            s.state = crate::net::SocketState::Connected;
                        });
                        return 0;
                    }
                    crate::net::tcp::TcpState::Closed => {
                        crate::net::tcp::destroy(tcp_idx);
                        crate::net::with_socket_mut(handle, |s| {
                            s.tcp_slot = None;
                            s.state = crate::net::SocketState::Closed;
                        });
                        return NEG_ECONNREFUSED;
                    }
                    _ => {
                        if crate::arch::x86_64::interrupts::tick_count().wrapping_sub(start_tick)
                            > 3000
                        {
                            // ~30 seconds timeout
                            crate::net::tcp::destroy(tcp_idx);
                            crate::net::with_socket_mut(handle, |s| {
                                s.tcp_slot = None;
                                s.state = crate::net::SocketState::Closed;
                            });
                            return NEG_ETIMEDOUT;
                        }
                        if has_pending_signal() {
                            return NEG_EINTR;
                        }
                        crate::task::yield_now();
                        restore_caller_context(pid, saved_user_rsp);
                    }
                }
            }
        }
        crate::net::SocketProtocol::Udp => {
            // Auto-bind an ephemeral port if not already bound
            let needs_bind = crate::net::with_socket(handle, |s| !s.udp_bound).unwrap_or(true);
            if needs_bind {
                let ephemeral = crate::arch::x86_64::interrupts::tick_count() as u16 | 0xC000;
                if crate::net::udp::bind(ephemeral) {
                    crate::net::with_socket_mut(handle, |s| {
                        s.local_port = ephemeral;
                        s.local_addr = crate::net::config::our_ip();
                        s.udp_bound = true;
                    });
                }
            }
            crate::net::with_socket_mut(handle, |s| {
                s.remote_addr = ip;
                s.remote_port = port;
                s.state = crate::net::SocketState::Connected;
            });
            0
        }
        crate::net::SocketProtocol::Icmp => {
            crate::net::with_socket_mut(handle, |s| {
                s.remote_addr = ip;
                s.state = crate::net::SocketState::Connected;
            });
            0
        }
    }
}

/// listen(fd, backlog) — syscall 50
fn sys_listen(fd: u64, backlog: u64) -> u64 {
    if let Ok((handle, _)) = unix_socket_handle_from_fd(fd) {
        return sys_listen_unix(handle, backlog);
    }
    let (handle, _kind, proto) = match socket_handle_from_fd(fd) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if !matches!(proto, crate::net::SocketProtocol::Tcp) {
        return NEG_EOPNOTSUPP;
    }
    let local_port = crate::net::with_socket(handle, |s| s.local_port).unwrap_or(0);
    if local_port == 0 {
        return NEG_EINVAL; // must bind first
    }
    // Allocate a TCP slot for listening
    let tcp_idx = match crate::net::tcp::create(local_port) {
        Some(idx) => idx,
        None => return NEG_EAGAIN,
    };
    crate::net::tcp::listen(tcp_idx);
    crate::net::with_socket_mut(handle, |s| {
        s.tcp_slot = Some(tcp_idx);
        s.state = crate::net::SocketState::Listening;
    });
    0
}

/// accept(fd, addr, addrlen) — syscall 43
fn sys_accept(fd: u64, addr_ptr: u64, addr_len_ptr: u64) -> u64 {
    if let Ok((_, _)) = unix_socket_handle_from_fd(fd) {
        return sys_accept_unix(fd, addr_ptr, addr_len_ptr, 0);
    }
    let (handle, _kind, _proto) = match socket_handle_from_fd(fd) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let tcp_idx = match crate::net::with_socket(handle, |s| s.tcp_slot) {
        Some(Some(idx)) => idx,
        _ => return NEG_EINVAL,
    };

    // Block until an incoming connection is established
    let pid = crate::process::current_pid();
    let saved_user_rsp = per_core_syscall_user_rsp();
    loop {
        let state = crate::net::tcp::state(tcp_idx);
        match state {
            crate::net::tcp::TcpState::Established | crate::net::tcp::TcpState::CloseWait => {
                // Connection accepted — the listen slot has been consumed.
                // Transfer it to a new socket.
                let new_handle = match crate::net::alloc_socket(
                    crate::net::SocketKind::Stream,
                    crate::net::SocketProtocol::Tcp,
                ) {
                    Some(h) => h,
                    None => return NEG_ENFILE,
                };

                // Get peer info from the TCP connection
                let (remote_ip, remote_port, local_port) =
                    crate::net::tcp::peer_info(tcp_idx).unwrap_or(([0; 4], 0, 0));

                crate::net::with_socket_mut(new_handle, |s| {
                    s.tcp_slot = Some(tcp_idx);
                    s.remote_addr = remote_ip;
                    s.remote_port = remote_port;
                    s.local_port = local_port;
                    s.local_addr = crate::net::config::our_ip();
                    s.state = crate::net::SocketState::Connected;
                });

                // Transfer ownership: clear old socket's tcp_slot first
                crate::net::with_socket_mut(handle, |s| {
                    s.tcp_slot = None;
                });

                // Create a new listen slot on the original socket
                let listen_port = crate::net::with_socket(handle, |s| s.local_port).unwrap_or(0);
                if let Some(new_tcp) = crate::net::tcp::create(listen_port) {
                    crate::net::tcp::listen(new_tcp);
                    crate::net::with_socket_mut(handle, |s| {
                        s.tcp_slot = Some(new_tcp);
                    });
                } else {
                    log::warn!(
                        "[socket] accept: no TCP slots for new listener on port {listen_port}"
                    );
                }

                // Write peer address to userspace
                if addr_ptr != 0 {
                    if addr_len_ptr == 0 {
                        // Linux requires addrlen when addr is non-null
                        crate::net::free_socket(new_handle);
                        return NEG_EINVAL;
                    }
                    let mut len_buf = [0u8; 4];
                    if crate::mm::user_mem::copy_from_user(&mut len_buf, addr_len_ptr).is_err() {
                        crate::net::free_socket(new_handle);
                        return NEG_EFAULT;
                    }
                    if u32::from_ne_bytes(len_buf) < 16 {
                        crate::net::free_socket(new_handle);
                        return NEG_EINVAL;
                    }
                    if let Err(e) = sockaddr_to_user(addr_ptr, remote_ip, remote_port) {
                        crate::net::free_socket(new_handle);
                        return e;
                    }
                }

                if addr_len_ptr != 0 {
                    let len_buf = 16u32.to_ne_bytes();
                    if crate::mm::user_mem::copy_to_user(addr_len_ptr, &len_buf).is_err() {
                        crate::net::free_socket(new_handle);
                        return NEG_EFAULT;
                    }
                }

                // Allocate fd for the new socket
                let entry = FdEntry {
                    backend: FdBackend::Socket { handle: new_handle },
                    offset: 0,
                    readable: true,
                    writable: true,
                    cloexec: false,
                    nonblock: false,
                };
                match alloc_fd(0, entry) {
                    Some(new_fd) => return new_fd as u64,
                    None => {
                        crate::net::free_socket(new_handle);
                        return NEG_EMFILE;
                    }
                }
            }
            _ => {
                if has_pending_signal() {
                    return NEG_EINTR;
                }
                crate::task::yield_now();
                restore_caller_context(pid, saved_user_rsp);
            }
        }
    }
}

/// accept4(fd, addr, addrlen, flags) — syscall 288
///
/// Like accept() but applies SOCK_NONBLOCK and SOCK_CLOEXEC flags
/// to the newly accepted socket FD.
fn sys_accept4(fd: u64, addr_ptr: u64, addr_len_ptr: u64, flags: u64) -> u64 {
    const SOCK_NONBLOCK: u64 = 0x800;
    const SOCK_CLOEXEC: u64 = 0x80000;
    if flags & !(SOCK_NONBLOCK | SOCK_CLOEXEC) != 0 {
        return NEG_EINVAL;
    }
    let result = sys_accept(fd, addr_ptr, addr_len_ptr);
    // If accept failed (negative), return the error.
    if result as i64 >= 0 {
        let new_fd = result as usize;
        if flags & (SOCK_NONBLOCK | SOCK_CLOEXEC) != 0 {
            with_current_fd_mut(new_fd, |slot| {
                if let Some(e) = slot {
                    if flags & SOCK_NONBLOCK != 0 {
                        e.nonblock = true;
                    }
                    if flags & SOCK_CLOEXEC != 0 {
                        e.cloexec = true;
                    }
                }
            });
        }
    }
    result
}

/// sendto(fd, buf, len, flags, addr, addrlen) — syscall 44
fn sys_sendto(fd: u64, buf_ptr: u64, len: u64, _flags: u64, addr_ptr: u64, addr_len: u64) -> u64 {
    let fd_idx = fd as usize;
    if fd_idx >= MAX_FDS {
        return NEG_EBADF;
    }
    let entry = match current_fd_entry(fd_idx) {
        Some(e) => e,
        None => return NEG_EBADF,
    };

    match &entry.backend {
        FdBackend::Socket { handle } => {
            let handle = *handle;
            let info = match crate::net::with_socket(handle, |s| {
                (
                    s.protocol,
                    s.tcp_slot,
                    s.remote_addr,
                    s.remote_port,
                    s.local_port,
                    s.shut_wr,
                )
            }) {
                Some(v) => v,
                None => return NEG_EBADF,
            };
            let (proto, tcp_slot, remote_addr, remote_port, local_port, shut_wr) = info;
            if shut_wr {
                const NEG_EPIPE: u64 = (-32_i64) as u64;
                return NEG_EPIPE;
            }

            let capped = (len as usize).min(4096);
            let mut tmp = [0u8; 4096];
            if crate::mm::user_mem::copy_from_user(&mut tmp[..capped], buf_ptr).is_err() {
                return NEG_EFAULT;
            }

            match proto {
                crate::net::SocketProtocol::Tcp => {
                    if let Some(tcp_idx) = tcp_slot {
                        crate::net::tcp::send(tcp_idx, &tmp[..capped]);
                        capped as u64
                    } else {
                        NEG_ENOTCONN
                    }
                }
                crate::net::SocketProtocol::Udp => {
                    // Use provided addr or connected peer
                    let (dst_ip, dst_port) = if addr_ptr != 0 {
                        if addr_len < 16 {
                            return NEG_EINVAL;
                        }
                        match sockaddr_from_user(addr_ptr) {
                            Ok(v) => v,
                            Err(e) => return e,
                        }
                    } else {
                        (remote_addr, remote_port)
                    };
                    if dst_port == 0 {
                        return NEG_ENOTCONN;
                    }
                    crate::net::udp::send(dst_ip, dst_port, local_port, &tmp[..capped]);
                    capped as u64
                }
                crate::net::SocketProtocol::Icmp => {
                    // Build and send ICMP echo request
                    let dst_ip = if addr_ptr != 0 {
                        if addr_len < 16 {
                            return NEG_EINVAL;
                        }
                        match sockaddr_from_user(addr_ptr) {
                            Ok((ip, _)) => ip,
                            Err(e) => return e,
                        }
                    } else {
                        remote_addr
                    };
                    // The payload IS the ICMP packet body (type/code/checksum/rest + data
                    // are built by the caller for raw ICMP, but for DGRAM ICMP sockets
                    // we build the echo request).
                    // Extract id and seq from the first 4 bytes if present
                    let (id, seq) = if capped >= 4 {
                        let id = u16::from_be_bytes([tmp[0], tmp[1]]);
                        let seq = u16::from_be_bytes([tmp[2], tmp[3]]);
                        (id, seq)
                    } else {
                        (1u16, 0u16)
                    };
                    let rest = [(id >> 8) as u8, id as u8, (seq >> 8) as u8, seq as u8];
                    let payload = if capped > 4 {
                        &tmp[4..capped]
                    } else {
                        &[0xABu8; 32] as &[u8]
                    };
                    use crate::net::icmp::{
                        ICMP_ECHO_REQUEST, PING_EXPECTED_ID, PING_EXPECTED_SEQ, PING_REPLY_RECEIVED,
                    };
                    use core::sync::atomic::Ordering;
                    PING_REPLY_RECEIVED.store(false, Ordering::Release);
                    PING_EXPECTED_ID.store(id, Ordering::Release);
                    PING_EXPECTED_SEQ.store(seq, Ordering::Release);
                    let icmp_pkt =
                        kernel_core::net::icmp::build(ICMP_ECHO_REQUEST, 0, rest, payload);
                    crate::net::ipv4::send(dst_ip, crate::net::ipv4::PROTO_ICMP, &icmp_pkt);
                    capped as u64
                }
            }
        }
        FdBackend::UnixSocket { handle } => {
            let handle = *handle;
            let sock_type = crate::net::unix::with_unix_socket(handle, |s| s.socket_type);
            match sock_type {
                Some(crate::net::unix::UnixSocketType::Datagram) => {
                    sys_sendto_unix(handle, buf_ptr, len, entry.nonblock, addr_ptr, addr_len)
                }
                Some(crate::net::unix::UnixSocketType::Stream) => {
                    // Stream sockets use write() semantics.
                    sys_linux_write(fd, buf_ptr, len)
                }
                None => NEG_EBADF,
            }
        }
        FdBackend::PipeWrite { .. } => {
            // sendto on pipe-based socketpair — delegate to write
            sys_linux_write(fd, buf_ptr, len)
        }
        _ => sys_linux_write(fd, buf_ptr, len),
    }
}

/// recvfrom(fd, buf, len, flags, addr, addrlen) — syscall 45
fn sys_recvfrom_socket(
    fd: u64,
    buf_ptr: u64,
    count: u64,
    flags: u64,
    addr_ptr: u64,
    addr_len_ptr: u64,
) -> u64 {
    const MSG_DONTWAIT: u64 = 0x40;

    let fd_idx = fd as usize;
    if fd_idx >= MAX_FDS {
        return NEG_EBADF;
    }
    let entry = match current_fd_entry(fd_idx) {
        Some(e) => e,
        None => return NEG_EBADF,
    };
    let nonblock = entry.nonblock || flags & MSG_DONTWAIT != 0;

    match &entry.backend {
        FdBackend::Socket { handle } => {
            let handle = *handle;
            let info = match crate::net::with_socket(handle, |s| {
                (
                    s.protocol,
                    s.tcp_slot,
                    s.local_port,
                    s.remote_addr,
                    s.remote_port,
                    s.shut_rd,
                )
            }) {
                Some(v) => v,
                None => return NEG_EBADF,
            };
            let (proto, tcp_slot, local_port, remote_addr, remote_port, shut_rd) = info;
            if shut_rd {
                return 0; // EOF
            }

            // Validate addr_len if addr_ptr is provided
            if addr_ptr != 0 {
                if addr_len_ptr == 0 {
                    return NEG_EINVAL;
                }
                let mut len_buf = [0u8; 4];
                if crate::mm::user_mem::copy_from_user(&mut len_buf, addr_len_ptr).is_err() {
                    return NEG_EFAULT;
                }
                if u32::from_ne_bytes(len_buf) < 16 {
                    return NEG_EINVAL;
                }
            }

            let capped = (count as usize).min(4096);

            match proto {
                crate::net::SocketProtocol::Tcp => {
                    let tcp_idx = match tcp_slot {
                        Some(idx) => idx,
                        None => return NEG_ENOTCONN,
                    };
                    let pid = crate::process::current_pid();
                    let saved_user_rsp = per_core_syscall_user_rsp();
                    loop {
                        let mut tmp = [0u8; 4096];
                        let n = crate::net::tcp::recv(tcp_idx, &mut tmp[..capped]);
                        if n > 0 {
                            if crate::mm::user_mem::copy_to_user(buf_ptr, &tmp[..n]).is_err() {
                                return NEG_EFAULT;
                            }
                            if addr_ptr != 0 {
                                if let Err(e) = sockaddr_to_user(addr_ptr, remote_addr, remote_port)
                                {
                                    return e;
                                }
                                if addr_len_ptr != 0 {
                                    let len_buf = 16u32.to_ne_bytes();
                                    if crate::mm::user_mem::copy_to_user(addr_len_ptr, &len_buf)
                                        .is_err()
                                    {
                                        return NEG_EFAULT;
                                    }
                                }
                            }
                            return n as u64;
                        }
                        // Check if connection is closed
                        let state = crate::net::tcp::state(tcp_idx);
                        if matches!(
                            state,
                            crate::net::tcp::TcpState::CloseWait
                                | crate::net::tcp::TcpState::Closed
                                | crate::net::tcp::TcpState::TimeWait
                        ) {
                            return 0; // EOF
                        }
                        if nonblock {
                            return NEG_EAGAIN;
                        }
                        if has_pending_signal() {
                            return NEG_EINTR;
                        }
                        crate::task::yield_now();
                        restore_caller_context(pid, saved_user_rsp);
                    }
                }
                crate::net::SocketProtocol::Udp => {
                    let pid = crate::process::current_pid();
                    let saved_user_rsp = per_core_syscall_user_rsp();
                    loop {
                        if let Some(dgram) = crate::net::udp::recv(local_port) {
                            let n = dgram.data.len().min(capped);
                            if crate::mm::user_mem::copy_to_user(buf_ptr, &dgram.data[..n]).is_err()
                            {
                                return NEG_EFAULT;
                            }
                            if addr_ptr != 0 {
                                if let Err(e) =
                                    sockaddr_to_user(addr_ptr, dgram.src_ip, dgram.src_port)
                                {
                                    return e;
                                }
                                if addr_len_ptr != 0 {
                                    let len_buf = 16u32.to_ne_bytes();
                                    if crate::mm::user_mem::copy_to_user(addr_len_ptr, &len_buf)
                                        .is_err()
                                    {
                                        return NEG_EFAULT;
                                    }
                                }
                            }
                            return n as u64;
                        }
                        if nonblock {
                            return NEG_EAGAIN;
                        }
                        if has_pending_signal() {
                            return NEG_EINTR;
                        }
                        crate::task::yield_now();
                        restore_caller_context(pid, saved_user_rsp);
                    }
                }
                crate::net::SocketProtocol::Icmp => {
                    // Wait for ICMP echo reply
                    use crate::net::icmp::{PING_REPLY_RECEIVED, PING_REPLY_TICK};
                    use core::sync::atomic::Ordering;
                    let pid = crate::process::current_pid();
                    let saved_user_rsp = per_core_syscall_user_rsp();
                    loop {
                        if PING_REPLY_RECEIVED.load(Ordering::Acquire) {
                            PING_REPLY_RECEIVED.store(false, Ordering::Release);
                            let tick = PING_REPLY_TICK.load(Ordering::Acquire);
                            // Write tick as 8-byte LE to userspace as reply data
                            let tick_bytes = tick.to_le_bytes();
                            let n = tick_bytes.len().min(capped);
                            if crate::mm::user_mem::copy_to_user(buf_ptr, &tick_bytes[..n]).is_err()
                            {
                                return NEG_EFAULT;
                            }
                            if addr_ptr != 0 {
                                if let Err(e) = sockaddr_to_user(addr_ptr, remote_addr, 0) {
                                    return e;
                                }
                                if addr_len_ptr != 0 {
                                    let len_buf = 16u32.to_ne_bytes();
                                    if crate::mm::user_mem::copy_to_user(addr_len_ptr, &len_buf)
                                        .is_err()
                                    {
                                        return NEG_EFAULT;
                                    }
                                }
                            }
                            return n as u64;
                        }
                        if nonblock {
                            return NEG_EAGAIN;
                        }
                        if has_pending_signal() {
                            return NEG_EINTR;
                        }
                        crate::task::yield_now();
                        restore_caller_context(pid, saved_user_rsp);
                    }
                }
            }
        }
        FdBackend::PipeRead { pipe_id } => {
            let pipe_id = *pipe_id;
            let len = (count as usize).min(4096);

            if nonblock {
                let mut tmp = [0u8; 4096];
                match crate::pipe::pipe_read(pipe_id, &mut tmp[..len]) {
                    Ok(n) if n > 0 => {
                        if crate::mm::user_mem::copy_to_user(buf_ptr, &tmp[..n]).is_err() {
                            return NEG_EFAULT;
                        }
                        n as u64
                    }
                    Ok(_) => 0,
                    Err(_) => NEG_EAGAIN,
                }
            } else {
                let pid = crate::process::current_pid();
                let saved_user_rsp = per_core_syscall_user_rsp();
                loop {
                    let mut tmp = [0u8; 4096];
                    match crate::pipe::pipe_read(pipe_id, &mut tmp[..len]) {
                        Ok(0) => return 0,
                        Ok(n) => {
                            if crate::mm::user_mem::copy_to_user(buf_ptr, &tmp[..n]).is_err() {
                                return NEG_EFAULT;
                            }
                            return n as u64;
                        }
                        Err(_) => {
                            if has_pending_signal() {
                                return NEG_EINTR;
                            }
                            crate::task::yield_now();
                            restore_caller_context(pid, saved_user_rsp);
                        }
                    }
                }
            }
        }
        FdBackend::UnixSocket { handle } => {
            let handle = *handle;
            let sock_type = crate::net::unix::with_unix_socket(handle, |s| s.socket_type);
            match sock_type {
                Some(crate::net::unix::UnixSocketType::Datagram) => {
                    sys_recvfrom_unix(handle, buf_ptr, count, nonblock, addr_ptr, addr_len_ptr)
                }
                Some(crate::net::unix::UnixSocketType::Stream) => {
                    sys_linux_read(fd, buf_ptr, count)
                }
                None => NEG_EBADF,
            }
        }
        _ => sys_linux_read(fd, buf_ptr, count),
    }
}

/// shutdown(fd, how) — syscall 48
fn sys_shutdown_sock(fd: u64, how: u64) -> u64 {
    if let Ok((handle, _)) = unix_socket_handle_from_fd(fd) {
        return sys_shutdown_unix(handle, how);
    }
    let (handle, _kind, _proto) = match socket_handle_from_fd(fd) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let tcp_slot = crate::net::with_socket(handle, |s| s.tcp_slot).flatten();
    match how {
        0 => {
            // SHUT_RD
            crate::net::with_socket_mut(handle, |s| s.shut_rd = true);
        }
        1 => {
            // SHUT_WR
            if let Some(tcp_idx) = tcp_slot {
                crate::net::tcp::close(tcp_idx); // send FIN
            }
            crate::net::with_socket_mut(handle, |s| s.shut_wr = true);
        }
        2 => {
            // SHUT_RDWR
            if let Some(tcp_idx) = tcp_slot {
                crate::net::tcp::close(tcp_idx);
            }
            crate::net::with_socket_mut(handle, |s| {
                s.shut_rd = true;
                s.shut_wr = true;
                s.state = crate::net::SocketState::Closed;
            });
        }
        _ => return NEG_EINVAL,
    }
    0
}

/// getsockname(fd, addr, addrlen) — syscall 51
fn sys_getsockname(fd: u64, addr_ptr: u64, addr_len_ptr: u64) -> u64 {
    let (handle, _kind, _proto) = match socket_handle_from_fd(fd) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let (ip, port) = match crate::net::with_socket(handle, |s| (s.local_addr, s.local_port)) {
        Some(v) => v,
        None => return NEG_EBADF,
    };
    if addr_len_ptr != 0 {
        let mut len_buf = [0u8; 4];
        if crate::mm::user_mem::copy_from_user(&mut len_buf, addr_len_ptr).is_err() {
            return NEG_EFAULT;
        }
        if u32::from_ne_bytes(len_buf) < 16 {
            return NEG_EINVAL;
        }
    }
    match sockaddr_to_user(addr_ptr, ip, port) {
        Ok(()) => {}
        Err(e) => return e,
    }
    if addr_len_ptr != 0 {
        let len_buf = 16u32.to_ne_bytes();
        if crate::mm::user_mem::copy_to_user(addr_len_ptr, &len_buf).is_err() {
            return NEG_EFAULT;
        }
    }
    0
}

/// getpeername(fd, addr, addrlen) — syscall 52
fn sys_getpeername(fd: u64, addr_ptr: u64, addr_len_ptr: u64) -> u64 {
    let (handle, _kind, _proto) = match socket_handle_from_fd(fd) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let info = match crate::net::with_socket(handle, |s| (s.remote_addr, s.remote_port, s.state)) {
        Some(v) => v,
        None => return NEG_EBADF,
    };
    let (ip, port, state) = info;
    if !matches!(state, crate::net::SocketState::Connected) {
        return NEG_ENOTCONN;
    }
    if addr_len_ptr != 0 {
        let mut len_buf = [0u8; 4];
        if crate::mm::user_mem::copy_from_user(&mut len_buf, addr_len_ptr).is_err() {
            return NEG_EFAULT;
        }
        if u32::from_ne_bytes(len_buf) < 16 {
            return NEG_EINVAL;
        }
    }
    match sockaddr_to_user(addr_ptr, ip, port) {
        Ok(()) => {}
        Err(e) => return e,
    }
    if addr_len_ptr != 0 {
        let len_buf = 16u32.to_ne_bytes();
        if crate::mm::user_mem::copy_to_user(addr_len_ptr, &len_buf).is_err() {
            return NEG_EFAULT;
        }
    }
    0
}

/// setsockopt(fd, level, optname, optval, optlen) — syscall 54
fn sys_setsockopt(fd: u64, level: u64, optname: u64, optval_ptr: u64, optlen: u64) -> u64 {
    let (handle, _kind, _proto) = match socket_handle_from_fd(fd) {
        Ok(v) => v,
        Err(e) => return e,
    };
    // Read the option value (up to 4 bytes for int options)
    if optlen < 4 {
        return NEG_EINVAL;
    }
    let val = if optval_ptr != 0 {
        let mut buf = [0u8; 4];
        if crate::mm::user_mem::copy_from_user(&mut buf, optval_ptr).is_err() {
            return NEG_EFAULT;
        }
        i32::from_ne_bytes(buf)
    } else {
        return NEG_EFAULT;
    };

    const SOL_SOCKET: u64 = 1;
    const SO_REUSEADDR: u64 = 2;
    const SO_KEEPALIVE: u64 = 9;
    const SO_RCVBUF: u64 = 8;
    const SO_SNDBUF: u64 = 7;
    const IPPROTO_TCP: u64 = 6;
    const TCP_NODELAY: u64 = 1;

    match (level, optname) {
        (SOL_SOCKET, SO_REUSEADDR) => {
            crate::net::with_socket_mut(handle, |s| s.options.reuse_addr = val != 0);
        }
        (SOL_SOCKET, SO_KEEPALIVE) => {
            crate::net::with_socket_mut(handle, |s| s.options.keep_alive = val != 0);
        }
        (SOL_SOCKET, SO_RCVBUF) => {
            crate::net::with_socket_mut(handle, |s| s.options.recv_buf_size = val as u32);
        }
        (SOL_SOCKET, SO_SNDBUF) => {
            crate::net::with_socket_mut(handle, |s| s.options.send_buf_size = val as u32);
        }
        (IPPROTO_TCP, TCP_NODELAY) => {
            crate::net::with_socket_mut(handle, |s| s.options.tcp_nodelay = val != 0);
        }
        _ => return NEG_ENOPROTOOPT,
    }
    0
}

/// getsockopt(fd, level, optname, optval, optlen) — syscall 55
fn sys_getsockopt(fd: u64, level: u64, optname: u64, optval_ptr: u64, optlen_ptr: u64) -> u64 {
    let (handle, _kind, _proto) = match socket_handle_from_fd(fd) {
        Ok(v) => v,
        Err(e) => return e,
    };

    const SOL_SOCKET: u64 = 1;
    const SO_REUSEADDR: u64 = 2;
    const SO_KEEPALIVE: u64 = 9;
    const SO_RCVBUF: u64 = 8;
    const SO_SNDBUF: u64 = 7;
    const IPPROTO_TCP: u64 = 6;
    const TCP_NODELAY: u64 = 1;

    let val: i32 = match (level, optname) {
        (SOL_SOCKET, SO_REUSEADDR) => {
            crate::net::with_socket(handle, |s| s.options.reuse_addr as i32).unwrap_or(0)
        }
        (SOL_SOCKET, SO_KEEPALIVE) => {
            crate::net::with_socket(handle, |s| s.options.keep_alive as i32).unwrap_or(0)
        }
        (SOL_SOCKET, SO_RCVBUF) => {
            crate::net::with_socket(handle, |s| s.options.recv_buf_size as i32).unwrap_or(0)
        }
        (SOL_SOCKET, SO_SNDBUF) => {
            crate::net::with_socket(handle, |s| s.options.send_buf_size as i32).unwrap_or(0)
        }
        (IPPROTO_TCP, TCP_NODELAY) => {
            crate::net::with_socket(handle, |s| s.options.tcp_nodelay as i32).unwrap_or(0)
        }
        _ => return NEG_ENOPROTOOPT,
    };

    // Validate caller's buffer size
    if optlen_ptr != 0 {
        let mut len_buf = [0u8; 4];
        if crate::mm::user_mem::copy_from_user(&mut len_buf, optlen_ptr).is_err() {
            return NEG_EFAULT;
        }
        let caller_len = u32::from_ne_bytes(len_buf);
        if caller_len < 4 {
            return NEG_EINVAL;
        }
    }

    if optval_ptr == 0 {
        return NEG_EFAULT;
    }
    let buf = val.to_ne_bytes();
    if crate::mm::user_mem::copy_to_user(optval_ptr, &buf).is_err() {
        return NEG_EFAULT;
    }
    if optlen_ptr != 0 {
        let len_buf = 4u32.to_ne_bytes();
        if crate::mm::user_mem::copy_to_user(optlen_ptr, &len_buf).is_err() {
            return NEG_EFAULT;
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Phase 37: I/O multiplexing helpers
// ---------------------------------------------------------------------------

const POLLIN: i16 = 0x001;
const POLLOUT: i16 = 0x004;
const POLLERR: i16 = 0x008;
const POLLHUP: i16 = 0x010;
const POLLNVAL: i16 = 0x020;

/// Query current readiness events for a file descriptor.
///
/// Returns a bitmask of POLLIN/POLLOUT/POLLHUP/POLLERR/POLLNVAL.
fn fd_poll_events(entry: &FdEntry) -> i16 {
    match &entry.backend {
        FdBackend::PipeRead { pipe_id } => {
            // Side-effect-free readiness check.
            let mut revents: i16 = 0;
            // POLLHUP when writer has closed (even if data remains in buffer).
            if crate::pipe::pipe_writer_closed(*pipe_id) {
                revents |= POLLHUP;
            }
            match crate::pipe::pipe_read_ready(*pipe_id) {
                Some(true) => revents |= POLLIN,
                Some(false) => {}
                None => revents |= POLLHUP,
            }
            revents
        }
        FdBackend::PipeWrite { pipe_id } => {
            // Side-effect-free writability check.
            match crate::pipe::pipe_write_ready(*pipe_id) {
                Some(true) => POLLOUT,     // writable
                Some(false) => 0,          // full but reader alive
                None => POLLERR | POLLHUP, // reader closed (EPIPE)
            }
        }
        FdBackend::DeviceTTY { .. } | FdBackend::Stdin => {
            let mut revents: i16 = 0;
            if entry.readable && crate::stdin::has_data() {
                revents |= POLLIN;
            }
            if entry.writable {
                revents |= POLLOUT;
            }
            revents
        }
        FdBackend::Socket { handle } => {
            let h = *handle;
            crate::net::with_socket(h, |s| {
                let mut revents: i16 = 0;
                let readable = match s.protocol {
                    crate::net::SocketProtocol::Tcp => {
                        if let Some(tcp_idx) = s.tcp_slot {
                            if matches!(s.state, crate::net::SocketState::Listening) {
                                matches!(
                                    crate::net::tcp::state(tcp_idx),
                                    crate::net::tcp::TcpState::Established
                                        | crate::net::tcp::TcpState::CloseWait
                                )
                            } else {
                                crate::net::tcp::has_recv_data(tcp_idx)
                                    || matches!(
                                        crate::net::tcp::state(tcp_idx),
                                        crate::net::tcp::TcpState::CloseWait
                                            | crate::net::tcp::TcpState::Closed
                                            | crate::net::tcp::TcpState::TimeWait
                                    )
                            }
                        } else {
                            false
                        }
                    }
                    crate::net::SocketProtocol::Udp => crate::net::udp::has_data(s.local_port),
                    crate::net::SocketProtocol::Icmp => crate::net::icmp::PING_REPLY_RECEIVED
                        .load(core::sync::atomic::Ordering::Acquire),
                };
                let writable = match s.protocol {
                    crate::net::SocketProtocol::Tcp => {
                        s.tcp_slot.is_some()
                            && matches!(s.state, crate::net::SocketState::Connected)
                    }
                    _ => true,
                };
                if readable {
                    revents |= POLLIN;
                }
                if writable {
                    revents |= POLLOUT;
                }
                if matches!(s.state, crate::net::SocketState::Closed) {
                    revents |= POLLHUP;
                }
                // TCP RST → POLLERR
                if let Some(tcp_idx) = s.tcp_slot
                    && matches!(
                        crate::net::tcp::state(tcp_idx),
                        crate::net::tcp::TcpState::Closed
                    )
                    && !matches!(
                        s.state,
                        crate::net::SocketState::Closed | crate::net::SocketState::Listening
                    )
                {
                    revents |= POLLERR;
                }
                revents
            })
            .unwrap_or(POLLNVAL)
        }
        FdBackend::PtyMaster { pty_id } => {
            let id = *pty_id;
            let table = crate::pty::PTY_TABLE.lock();
            if let Some(Some(pair)) = table.get(id as usize) {
                let mut revents: i16 = 0;
                if !pair.s2m.is_empty() {
                    revents |= POLLIN;
                }
                if pair.slave_refcount == 0 && pair.slave_opened {
                    revents |= POLLHUP | POLLIN;
                }
                if !pair.m2s.is_full() {
                    revents |= POLLOUT;
                }
                revents
            } else {
                POLLHUP
            }
        }
        FdBackend::PtySlave { pty_id } => {
            let id = *pty_id;
            let table = crate::pty::PTY_TABLE.lock();
            if let Some(Some(pair)) = table.get(id as usize) {
                let mut revents: i16 = 0;
                if pair.termios.is_canonical() {
                    if pair.edit_buf.as_slice().contains(&b'\n') || pair.eof_pending {
                        revents |= POLLIN;
                    }
                } else if !pair.m2s.is_empty() {
                    revents |= POLLIN;
                }
                if pair.master_refcount == 0 {
                    revents |= POLLHUP | POLLIN;
                }
                if !pair.s2m.is_full() {
                    revents |= POLLOUT;
                }
                revents
            } else {
                POLLHUP
            }
        }
        FdBackend::Stdout => POLLOUT,
        FdBackend::DevNull => POLLIN | POLLOUT,
        FdBackend::DevZero
        | FdBackend::DevUrandom
        | FdBackend::DevFull
        | FdBackend::Proc { .. }
        | FdBackend::Ramdisk { .. }
        | FdBackend::Tmpfs { .. }
        | FdBackend::Fat32Disk { .. }
        | FdBackend::Ext2Disk { .. }
        | FdBackend::Dir { .. } => POLLIN | POLLOUT,
        FdBackend::UnixSocket { handle } => {
            let h = *handle;
            // Extract socket info under the lock, then check peer separately
            // to avoid nested lock acquisition (deadlock).
            let info = crate::net::unix::with_unix_socket(h, |s| {
                (
                    s.socket_type,
                    s.state,
                    s.peer,
                    !s.recv_buf.is_empty(),
                    !s.backlog.is_empty(),
                    !s.dgram_queue.is_empty(),
                    s.shut_rd,
                )
            });
            match info {
                Some((sock_type, state, peer, has_data, has_backlog, has_dgram, shut_rd)) => {
                    let mut revents: i16 = 0;
                    match sock_type {
                        crate::net::unix::UnixSocketType::Stream => {
                            if has_data {
                                revents |= POLLIN;
                            }
                            if matches!(state, crate::net::unix::UnixSocketState::Listening)
                                && has_backlog
                            {
                                revents |= POLLIN;
                            }
                            if let Some(peer_h) = peer {
                                let peer_has_space =
                                    crate::net::unix::with_unix_socket(peer_h, |ps| {
                                        ps.recv_buf.len() < crate::net::unix::UNIX_STREAM_BUF_SIZE
                                    });
                                if peer_has_space.unwrap_or(false) {
                                    revents |= POLLOUT;
                                }
                                if peer_has_space.is_none() {
                                    revents |= POLLHUP; // peer freed
                                }
                            } else if matches!(state, crate::net::unix::UnixSocketState::Connected)
                            {
                                revents |= POLLHUP;
                            }
                        }
                        crate::net::unix::UnixSocketType::Datagram => {
                            if has_dgram {
                                revents |= POLLIN;
                            }
                            // Datagram writability is not determined by the local
                            // receive queue — always report writable.
                            revents |= POLLOUT;
                        }
                    }
                    if shut_rd {
                        revents |= POLLIN;
                    }
                    if matches!(state, crate::net::unix::UnixSocketState::Closed) {
                        revents |= POLLHUP;
                    }
                    revents
                }
                None => POLLNVAL,
            }
        }
        FdBackend::Epoll { .. } => 0, // epoll FDs not themselves pollable
    }
}

/// Register the current task on the wait queue(s) of a file descriptor.
///
/// Uses `WaitQueue::register()` to add the task without blocking, allowing
/// registration on multiple queues before a single block call.
/// Returns `true` if a wait queue was registered, `false` for non-pollable types.
fn fd_register_waiter(
    entry: &FdEntry,
    task_id: crate::task::TaskId,
    woken: &alloc::sync::Arc<core::sync::atomic::AtomicBool>,
) -> bool {
    match &entry.backend {
        FdBackend::PipeRead { pipe_id } | FdBackend::PipeWrite { pipe_id } => {
            let wqs = crate::pipe::PIPE_WAITQUEUES.lock();
            if let Some(Some(wq)) = wqs.get(*pipe_id) {
                wq.register(task_id, woken);
                return true;
            }
            false
        }
        FdBackend::DeviceTTY { .. } | FdBackend::Stdin => {
            crate::stdin::STDIN_WAITQUEUE.register(task_id, woken);
            true
        }
        FdBackend::Socket { handle } => {
            crate::net::SOCKET_WAITQUEUES[*handle as usize].register(task_id, woken);
            true
        }
        FdBackend::UnixSocket { handle } => {
            crate::net::unix::UNIX_SOCKET_WAITQUEUES[*handle].register(task_id, woken);
            true
        }
        FdBackend::PtyMaster { pty_id } => {
            crate::pty::PTY_MASTER_WQ[*pty_id as usize].register(task_id, woken);
            true
        }
        FdBackend::PtySlave { pty_id } => {
            crate::pty::PTY_SLAVE_WQ[*pty_id as usize].register(task_id, woken);
            true
        }
        _ => false, // non-pollable types (ramdisk, tmpfs, etc.)
    }
}

/// Deregister the current task from all wait queues it was registered on.
fn fd_deregister_waiter(entry: &FdEntry, task_id: crate::task::TaskId) {
    match &entry.backend {
        FdBackend::PipeRead { pipe_id } | FdBackend::PipeWrite { pipe_id } => {
            let wqs = crate::pipe::PIPE_WAITQUEUES.lock();
            if let Some(Some(wq)) = wqs.get(*pipe_id) {
                wq.deregister(task_id);
            }
        }
        FdBackend::DeviceTTY { .. } | FdBackend::Stdin => {
            crate::stdin::STDIN_WAITQUEUE.deregister(task_id);
        }
        FdBackend::Socket { handle } => {
            crate::net::SOCKET_WAITQUEUES[*handle as usize].deregister(task_id);
        }
        FdBackend::UnixSocket { handle } => {
            crate::net::unix::UNIX_SOCKET_WAITQUEUES[*handle].deregister(task_id);
        }
        FdBackend::PtyMaster { pty_id } => {
            crate::pty::PTY_MASTER_WQ[*pty_id as usize].deregister(task_id);
        }
        FdBackend::PtySlave { pty_id } => {
            crate::pty::PTY_SLAVE_WQ[*pty_id as usize].deregister(task_id);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Phase 37: poll(fds, nfds, timeout) — syscall 7
// ---------------------------------------------------------------------------

/// poll(fds, nfds, timeout) — wait-queue-driven I/O readiness notification.
///
/// Phase 37 rewrite: uses per-FD wait queues instead of busy-wait yield loop.
/// The task blocks via WaitQueue until an FD becomes ready or timeout expires.
#[allow(clippy::needless_range_loop)]
fn sys_poll(fds_ptr: u64, nfds: u64, timeout: u64) -> u64 {
    let nfds = nfds as usize;
    if nfds > 256 {
        return NEG_EINVAL;
    }
    let timeout_i = timeout as i64;
    // Convert ms timeout to tick deadline. ~100 Hz timer → 10ms/tick.
    let start_tick = crate::arch::x86_64::interrupts::tick_count();
    let deadline_tick = if timeout_i > 0 {
        Some(start_tick + (timeout_i as u64).div_ceil(10)) // round up
    } else {
        None // 0 = non-blocking, -1 = indefinite
    };
    let pid = crate::process::current_pid();
    let saved_user_rsp = per_core_syscall_user_rsp();

    // Read all pollfd structs from userspace once.
    let mut pfds = [[0u8; 8]; 256];
    for i in 0..nfds {
        let base = match fds_ptr.checked_add((i * 8) as u64) {
            Some(a) => a,
            None => return NEG_EFAULT,
        };
        if crate::mm::user_mem::copy_from_user(&mut pfds[i], base).is_err() {
            return NEG_EFAULT;
        }
    }

    // Collect FD entries for wait queue registration.
    let mut entries: [Option<FdEntry>; 256] = [const { None }; 256];
    for i in 0..nfds {
        let fd = i32::from_ne_bytes([pfds[i][0], pfds[i][1], pfds[i][2], pfds[i][3]]);
        if fd >= 0 && (fd as usize) < MAX_FDS {
            entries[i] = current_fd_entry(fd as usize);
        }
    }

    loop {
        let mut ready_count = 0u64;

        for i in 0..nfds {
            let events = i16::from_ne_bytes([pfds[i][4], pfds[i][5]]);
            let fd = i32::from_ne_bytes([pfds[i][0], pfds[i][1], pfds[i][2], pfds[i][3]]);
            let mut revents: i16 = 0;

            if fd >= 0 {
                if let Some(entry) = &entries[i] {
                    let ready = fd_poll_events(entry);
                    // Report only events the caller asked for, plus unconditional ones.
                    revents = (ready & events) | (ready & (POLLHUP | POLLERR));
                } else {
                    revents = POLLNVAL;
                }
            }

            if revents != 0 {
                ready_count += 1;
            }
            pfds[i][6..8].copy_from_slice(&revents.to_ne_bytes());
        }

        // Fast path: something ready, non-blocking poll, or timeout expired.
        let timed_out =
            deadline_tick.is_some_and(|d| crate::arch::x86_64::interrupts::tick_count() >= d);
        if ready_count > 0 || timeout_i == 0 || timed_out {
            // Write results back to userspace.
            for i in 0..nfds {
                let base = match fds_ptr.checked_add((i * 8) as u64) {
                    Some(a) => a,
                    None => return NEG_EFAULT,
                };
                if crate::mm::user_mem::copy_to_user(base, &pfds[i]).is_err() {
                    return NEG_EFAULT;
                }
            }
            return ready_count;
        }

        if has_pending_signal() {
            return NEG_EINTR;
        }

        // Nothing ready — register on all FD wait queues and block.
        let task_id = match crate::task::scheduler::current_task_id() {
            Some(id) => id,
            None => return NEG_EINTR,
        };
        let woken = alloc::sync::Arc::new(core::sync::atomic::AtomicBool::new(false));

        let mut registered_any = false;
        for i in 0..nfds {
            if let Some(entry) = &entries[i]
                && fd_register_waiter(entry, task_id, &woken)
            {
                registered_any = true;
            }
        }

        // If no FDs could be registered (all non-pollable), yield to avoid
        // blocking forever with no wake source.
        if !registered_any {
            crate::task::yield_now();
            restore_caller_context(pid, saved_user_rsp);
            continue;
        }

        // Re-check readiness after registration to close the TOCTOU window.
        // If an event arrived between the first scan and registration, the
        // woken flag may not be set, so we must re-scan before blocking.
        let mut any_ready = false;
        for i in 0..nfds {
            if let Some(entry) = &entries[i] {
                let events = i16::from_ne_bytes([pfds[i][4], pfds[i][5]]);
                let ready = fd_poll_events(entry);
                if (ready & events) != 0 || (ready & (POLLHUP | POLLERR)) != 0 {
                    any_ready = true;
                    break;
                }
            }
        }

        if any_ready {
            // Something became ready — deregister and re-scan on next loop iteration.
            for i in 0..nfds {
                if let Some(entry) = &entries[i] {
                    fd_deregister_waiter(entry, task_id);
                }
            }
            continue;
        }

        // Block until woken by an FD event. For positive timeouts, use
        // yield instead of full block so the tick counter advances and the
        // deadline check at the top of the loop can fire.
        if deadline_tick.is_some() {
            // Positive timeout: yield once to let timer ticks advance.
            for i in 0..nfds {
                if let Some(entry) = &entries[i] {
                    fd_deregister_waiter(entry, task_id);
                }
            }
            crate::task::yield_now();
            restore_caller_context(pid, saved_user_rsp);
        } else {
            // Indefinite timeout (-1): block on wait queues.
            crate::task::scheduler::block_current_unless_woken(&woken);
            restore_caller_context(pid, saved_user_rsp);
            for i in 0..nfds {
                if let Some(entry) = &entries[i] {
                    fd_deregister_waiter(entry, task_id);
                }
            }
        }

        // Re-scan on next iteration of the loop.
    }
}

// ---------------------------------------------------------------------------
// Phase 37: select(nfds, readfds, writefds, exceptfds, timeout) — syscall 23
// ---------------------------------------------------------------------------

/// Read an fd_set bitmap from userspace. Returns a 32-bit mask (one bit per FD).
fn read_fd_set(ptr: u64, nfds: usize) -> Result<u32, u64> {
    if ptr == 0 {
        return Ok(0);
    }
    // fd_set is 128 bytes (1024 bits). We only need the first 4 bytes (32 FDs).
    let mut buf = [0u8; 4];
    if crate::mm::user_mem::copy_from_user(&mut buf, ptr).is_err() {
        return Err(NEG_EFAULT);
    }
    let mask = u32::from_ne_bytes(buf);
    // Clear bits beyond nfds. Handle nfds >= 32 without shift overflow.
    let keep = if nfds >= 32 {
        u32::MAX
    } else {
        (1u32 << nfds) - 1
    };
    Ok(mask & keep)
}

/// Write an fd_set bitmap back to userspace.
fn write_fd_set(ptr: u64, mask: u32) -> Result<(), u64> {
    if ptr == 0 {
        return Ok(());
    }
    // Zero the entire 128-byte fd_set, then write our 4 bytes.
    let zero = [0u8; 128];
    if crate::mm::user_mem::copy_to_user(ptr, &zero).is_err() {
        return Err(NEG_EFAULT);
    }
    let buf = mask.to_ne_bytes();
    if crate::mm::user_mem::copy_to_user(ptr, &buf).is_err() {
        return Err(NEG_EFAULT);
    }
    Ok(())
}

/// select(nfds, readfds, writefds, exceptfds, timeout) — syscall 23
fn sys_select(
    nfds: u64,
    readfds_ptr: u64,
    writefds_ptr: u64,
    exceptfds_ptr: u64,
    timeout_ptr: u64,
) -> u64 {
    // Parse timeval timeout: NULL = block indefinitely, {0,0} = non-blocking.
    let timeout_ms: Option<u64> = if timeout_ptr == 0 {
        None
    } else {
        let mut tv = [0u8; 16]; // struct timeval: tv_sec (8) + tv_usec (8)
        if crate::mm::user_mem::copy_from_user(&mut tv, timeout_ptr).is_err() {
            return NEG_EFAULT;
        }
        let sec = i64::from_ne_bytes(tv[0..8].try_into().unwrap());
        let usec = i64::from_ne_bytes(tv[8..16].try_into().unwrap());
        if sec < 0 || !(0..1_000_000).contains(&usec) {
            return NEG_EINVAL;
        }
        Some(sec as u64 * 1000 + usec as u64 / 1000)
    };
    select_inner(nfds, readfds_ptr, writefds_ptr, exceptfds_ptr, timeout_ms)
}

/// Shared select implementation used by both select() and pselect6().
#[allow(clippy::needless_range_loop)]
fn select_inner(
    nfds: u64,
    readfds_ptr: u64,
    writefds_ptr: u64,
    exceptfds_ptr: u64,
    timeout_ms: Option<u64>,
) -> u64 {
    let nfds = (nfds as usize).min(MAX_FDS);

    // Read fd_set bitmaps.
    let read_mask = match read_fd_set(readfds_ptr, nfds) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let write_mask = match read_fd_set(writefds_ptr, nfds) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let except_mask = match read_fd_set(exceptfds_ptr, nfds) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let start_tick = crate::arch::x86_64::interrupts::tick_count();
    let deadline_tick = timeout_ms
        .filter(|&ms| ms > 0)
        .map(|ms| start_tick + ms.div_ceil(10));
    let nonblocking = timeout_ms == Some(0);

    let combined = read_mask | write_mask | except_mask;
    if combined == 0 && nonblocking {
        // No FDs and non-blocking → return immediately.
        return 0;
    }

    // Collect FD entries.
    let mut entries: [Option<FdEntry>; 32] = [const { None }; 32];
    for fd in 0..nfds {
        if combined & (1 << fd) != 0 {
            entries[fd] = current_fd_entry(fd);
        }
    }

    let pid = crate::process::current_pid();
    let saved_user_rsp = per_core_syscall_user_rsp();

    loop {
        let mut r_out: u32 = 0;
        let mut w_out: u32 = 0;
        let mut e_out: u32 = 0;

        for fd in 0..nfds {
            let bit = 1u32 << fd;
            if combined & bit == 0 {
                continue;
            }
            if let Some(entry) = &entries[fd] {
                let ready = fd_poll_events(entry);
                if read_mask & bit != 0 && ready & POLLIN != 0 {
                    r_out |= bit;
                }
                if write_mask & bit != 0 && ready & POLLOUT != 0 {
                    w_out |= bit;
                }
                if except_mask & bit != 0 && ready & POLLERR != 0 {
                    e_out |= bit;
                }
            } else {
                return NEG_EBADF;
            }
        }

        let total = (r_out.count_ones() + w_out.count_ones() + e_out.count_ones()) as u64;

        let timed_out =
            deadline_tick.is_some_and(|d| crate::arch::x86_64::interrupts::tick_count() >= d);
        if total > 0 || nonblocking || timed_out {
            // Write results back.
            if let Err(e) = write_fd_set(readfds_ptr, r_out) {
                return e;
            }
            if let Err(e) = write_fd_set(writefds_ptr, w_out) {
                return e;
            }
            if let Err(e) = write_fd_set(exceptfds_ptr, e_out) {
                return e;
            }
            return total;
        }

        if has_pending_signal() {
            return NEG_EINTR;
        }

        // Block on wait queues.
        let task_id = match crate::task::scheduler::current_task_id() {
            Some(id) => id,
            None => return NEG_EINTR,
        };
        let woken = alloc::sync::Arc::new(core::sync::atomic::AtomicBool::new(false));
        let mut registered_any = false;
        for fd in 0..nfds {
            if let Some(entry) = &entries[fd]
                && combined & (1 << fd) != 0
                && fd_register_waiter(entry, task_id, &woken)
            {
                registered_any = true;
            }
        }

        if !registered_any {
            crate::task::yield_now();
            restore_caller_context(pid, saved_user_rsp);
            continue;
        }

        // Re-check readiness after registration to close the TOCTOU window.
        let mut any_ready = false;
        for fd in 0..nfds {
            if let Some(entry) = &entries[fd]
                && combined & (1 << fd) != 0
            {
                let ready = fd_poll_events(entry);
                if (ready & POLLIN) != 0
                    || (ready & POLLOUT) != 0
                    || (ready & (POLLHUP | POLLERR)) != 0
                {
                    any_ready = true;
                    break;
                }
            }
        }
        if any_ready {
            for fd in 0..nfds {
                if let Some(entry) = &entries[fd]
                    && combined & (1 << fd) != 0
                {
                    fd_deregister_waiter(entry, task_id);
                }
            }
            continue;
        }

        if deadline_tick.is_some() {
            // Positive timeout: yield to let timer ticks advance.
            for fd in 0..nfds {
                if let Some(entry) = &entries[fd]
                    && combined & (1 << fd) != 0
                {
                    fd_deregister_waiter(entry, task_id);
                }
            }
            crate::task::yield_now();
            restore_caller_context(pid, saved_user_rsp);
        } else {
            // Indefinite timeout (NULL): block on wait queues.
            crate::task::scheduler::block_current_unless_woken(&woken);
            restore_caller_context(pid, saved_user_rsp);
            for fd in 0..nfds {
                if let Some(entry) = &entries[fd]
                    && combined & (1 << fd) != 0
                {
                    fd_deregister_waiter(entry, task_id);
                }
            }
        }
    }
}

/// pselect6(nfds, readfds, writefds, exceptfds, timeout, sigmask_ptr) — syscall 270
///
/// Modern variant of select with timespec timeout and signal mask.
/// Signal mask is accepted but not applied (no signal masking yet).
fn sys_pselect6(
    nfds: u64,
    readfds_ptr: u64,
    writefds_ptr: u64,
    exceptfds_ptr: u64,
    timeout_ptr: u64,
) -> u64 {
    // pselect6 uses timespec {tv_sec, tv_nsec}. Parse to timeout_ms without
    // mutating user memory. The 6th arg (sigmask) is ignored.
    let timeout_ms: Option<u64> = if timeout_ptr != 0 {
        let mut ts = [0u8; 16];
        if crate::mm::user_mem::copy_from_user(&mut ts, timeout_ptr).is_err() {
            return NEG_EFAULT;
        }
        let sec = i64::from_ne_bytes(ts[0..8].try_into().unwrap());
        let nsec = i64::from_ne_bytes(ts[8..16].try_into().unwrap());
        if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
            return NEG_EINVAL;
        }
        Some(sec as u64 * 1000 + nsec as u64 / 1_000_000)
    } else {
        None // block indefinitely
    };
    select_inner(nfds, readfds_ptr, writefds_ptr, exceptfds_ptr, timeout_ms)
}

// ---------------------------------------------------------------------------
// Phase 37: epoll interface — syscalls 291, 233, 232
// ---------------------------------------------------------------------------

const MAX_EPOLL_INSTANCES: usize = 16;
const MAX_EPOLL_INTEREST: usize = 32;

#[allow(dead_code)]
const EPOLLIN: u32 = 0x001;
#[allow(dead_code)]
const EPOLLOUT: u32 = 0x004;
const EPOLLERR: u32 = 0x008;
const EPOLLHUP: u32 = 0x010;

const EPOLL_CTL_ADD: u64 = 1;
const EPOLL_CTL_DEL: u64 = 2;
const EPOLL_CTL_MOD: u64 = 3;

const EPOLL_CLOEXEC: u64 = 0x80000;

/// An entry in the epoll interest list: which FD to monitor and for what events.
#[derive(Clone)]
struct EpollInterest {
    fd: usize,
    events: u32,
    data: u64,
}

/// An epoll instance — tracks the interest set (which FDs to monitor).
/// Blocking in epoll_wait is done by registering on monitored FDs' wait queues.
struct EpollInstance {
    interests: alloc::vec::Vec<EpollInterest>,
    refcount: u32,
    owner_pid: crate::process::Pid,
}

impl EpollInstance {
    fn new(pid: crate::process::Pid) -> Self {
        Self {
            interests: alloc::vec::Vec::new(),
            refcount: 1,
            owner_pid: pid,
        }
    }
}

static EPOLL_TABLE: spin::Mutex<[Option<EpollInstance>; MAX_EPOLL_INSTANCES]> = {
    const NONE: Option<EpollInstance> = None;
    spin::Mutex::new([NONE; MAX_EPOLL_INSTANCES])
};

/// Public entry point for epoll_free (called from close_cloexec_fds / close_all_fds).
pub fn epoll_free_pub(instance_id: usize) {
    epoll_free(instance_id);
}

/// Public entry point for epoll_add_ref (called from add_fd_refs on fork/dup).
pub fn epoll_add_ref_pub(instance_id: usize) {
    epoll_add_ref(instance_id);
}

/// Decrement epoll instance refcount; free when it reaches zero.
fn epoll_free(instance_id: usize) {
    let mut table = EPOLL_TABLE.lock();
    if let Some(inst) = table.get_mut(instance_id).and_then(|slot| slot.as_mut()) {
        inst.refcount = inst.refcount.saturating_sub(1);
        if inst.refcount == 0 {
            table[instance_id] = None;
        }
    }
}

/// Increment epoll instance refcount (called on dup/fork).
fn epoll_add_ref(instance_id: usize) {
    let mut table = EPOLL_TABLE.lock();
    if let Some(inst) = table.get_mut(instance_id).and_then(|slot| slot.as_mut()) {
        inst.refcount += 1;
    }
}

/// Remove an FD from epoll interest lists owned by the current process.
fn epoll_remove_fd(fd: usize) {
    let pid = crate::process::current_pid();
    let mut table = EPOLL_TABLE.lock();
    for inst in table.iter_mut().flatten() {
        if inst.owner_pid == pid {
            inst.interests.retain(|i| i.fd != fd);
        }
    }
}

/// epoll_create1(flags) — syscall 291
fn sys_epoll_create1(flags: u64) -> u64 {
    // Reject unknown flags.
    if flags & !EPOLL_CLOEXEC != 0 {
        return NEG_EINVAL;
    }
    let cloexec = flags & EPOLL_CLOEXEC != 0;

    // Allocate an instance.
    let pid = crate::process::current_pid();
    let instance_id = {
        let mut table = EPOLL_TABLE.lock();
        let mut found = None;
        for (i, slot) in table.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(EpollInstance::new(pid));
                found = Some(i);
                break;
            }
        }
        match found {
            Some(id) => id,
            None => return NEG_ENOMEM,
        }
    };

    let entry = FdEntry {
        backend: FdBackend::Epoll { instance_id },
        offset: 0,
        readable: true,
        writable: false,
        cloexec,
        nonblock: false,
    };
    match alloc_fd(0, entry) {
        Some(fd) => fd as u64,
        None => {
            epoll_free(instance_id);
            NEG_EMFILE
        }
    }
}

/// epoll_ctl(epfd, op, fd, event_ptr) — syscall 233
fn sys_epoll_ctl(epfd: u64, op: u64, fd: u64, event_ptr: u64) -> u64 {
    let epfd_idx = epfd as usize;
    let fd_idx = fd as usize;
    if epfd_idx >= MAX_FDS || fd_idx >= MAX_FDS {
        return NEG_EBADF;
    }

    // Reject adding an epoll FD to itself (Linux returns EINVAL for this).
    if epfd_idx == fd_idx {
        return NEG_EINVAL;
    }

    // Get the epoll instance ID from the FD.
    let instance_id = match current_fd_entry(epfd_idx) {
        Some(e) => match e.backend {
            FdBackend::Epoll { instance_id } => instance_id,
            _ => return NEG_EINVAL,
        },
        None => return NEG_EBADF,
    };

    // Verify target FD exists and is not an epoll FD.
    match current_fd_entry(fd_idx) {
        Some(e) => {
            if matches!(e.backend, FdBackend::Epoll { .. }) {
                return NEG_EINVAL; // epoll FDs cannot be monitored
            }
        }
        None => return NEG_EBADF,
    }

    // Read epoll_event from userspace: packed { events: u32, data: u64 } = 12 bytes
    let (events, data) = if op != EPOLL_CTL_DEL {
        if event_ptr == 0 {
            return NEG_EFAULT;
        }
        let mut ev = [0u8; 12];
        if crate::mm::user_mem::copy_from_user(&mut ev, event_ptr).is_err() {
            return NEG_EFAULT;
        }
        let events = u32::from_ne_bytes([ev[0], ev[1], ev[2], ev[3]]);
        let data = u64::from_ne_bytes([ev[4], ev[5], ev[6], ev[7], ev[8], ev[9], ev[10], ev[11]]);
        (events, data)
    } else {
        (0, 0)
    };

    let mut table = EPOLL_TABLE.lock();
    let inst = match table.get_mut(instance_id).and_then(|s| s.as_mut()) {
        Some(i) => i,
        None => return NEG_EBADF,
    };

    match op {
        EPOLL_CTL_ADD => {
            // Check for duplicate.
            if inst.interests.iter().any(|i| i.fd == fd_idx) {
                return NEG_EEXIST;
            }
            if inst.interests.len() >= MAX_EPOLL_INTEREST {
                return NEG_ENOMEM;
            }
            inst.interests.push(EpollInterest {
                fd: fd_idx,
                events,
                data,
            });
            0
        }
        EPOLL_CTL_MOD => {
            if let Some(entry) = inst.interests.iter_mut().find(|i| i.fd == fd_idx) {
                entry.events = events;
                entry.data = data;
                0
            } else {
                const NEG_ENOENT: u64 = (-2_i64) as u64;
                NEG_ENOENT
            }
        }
        EPOLL_CTL_DEL => {
            let before = inst.interests.len();
            inst.interests.retain(|i| i.fd != fd_idx);
            if inst.interests.len() == before {
                const NEG_ENOENT: u64 = (-2_i64) as u64;
                NEG_ENOENT
            } else {
                0
            }
        }
        _ => NEG_EINVAL,
    }
}

/// epoll_wait(epfd, events, maxevents, timeout) — syscall 232
fn sys_epoll_wait(epfd: u64, events_ptr: u64, maxevents: u64, timeout: u64) -> u64 {
    let maxevents = (maxevents as usize).min(MAX_EPOLL_INTEREST);
    if maxevents == 0 {
        return NEG_EINVAL;
    }
    let timeout_i = timeout as i64;
    let start_tick = crate::arch::x86_64::interrupts::tick_count();
    let deadline_tick = if timeout_i > 0 {
        Some(start_tick + (timeout_i as u64).div_ceil(10))
    } else {
        None
    };

    let epfd_idx = epfd as usize;
    if epfd_idx >= MAX_FDS {
        return NEG_EBADF;
    }
    let instance_id = match current_fd_entry(epfd_idx) {
        Some(e) => match e.backend {
            FdBackend::Epoll { instance_id } => instance_id,
            _ => return NEG_EINVAL,
        },
        None => return NEG_EBADF,
    };

    let pid = crate::process::current_pid();
    let saved_user_rsp = per_core_syscall_user_rsp();

    loop {
        // Scan interest list for ready FDs.
        let mut out_count = 0usize;
        let interests = {
            let table = EPOLL_TABLE.lock();
            match table.get(instance_id).and_then(|s| s.as_ref()) {
                Some(inst) => inst.interests.clone(),
                None => return NEG_EBADF,
            }
        };

        for interest in &interests {
            if out_count >= maxevents {
                break;
            }
            if let Some(entry) = current_fd_entry(interest.fd) {
                let ready = fd_poll_events(&entry);
                let ready_u32 = ready as u16 as u32;
                let matched = ready_u32 & interest.events;
                // Also report unconditional events.
                let unconditional = ready_u32 & (EPOLLHUP | EPOLLERR);
                if matched != 0 || unconditional != 0 {
                    // Write epoll_event to userspace: packed { events: u32, data: u64 } = 12 bytes
                    let base = match events_ptr.checked_add((out_count * 12) as u64) {
                        Some(a) => a,
                        None => return NEG_EFAULT,
                    };
                    let ev_out = (matched | unconditional).to_ne_bytes();
                    let data_out = interest.data.to_ne_bytes();
                    let mut buf = [0u8; 12];
                    buf[0..4].copy_from_slice(&ev_out);
                    buf[4..12].copy_from_slice(&data_out);
                    if crate::mm::user_mem::copy_to_user(base, &buf).is_err() {
                        return NEG_EFAULT;
                    }
                    out_count += 1;
                }
            }
        }

        let timed_out =
            deadline_tick.is_some_and(|d| crate::arch::x86_64::interrupts::tick_count() >= d);
        if out_count > 0 || timeout_i == 0 || timed_out {
            return out_count as u64;
        }

        if has_pending_signal() {
            return NEG_EINTR;
        }

        // If the interest list is empty, there's nothing to wait on.
        // Return 0 immediately rather than blocking forever.
        if interests.is_empty() {
            return 0;
        }

        // Block on each monitored FD's wait queue so we wake on events.
        let task_id = match crate::task::scheduler::current_task_id() {
            Some(id) => id,
            None => return NEG_EINTR,
        };
        let woken = alloc::sync::Arc::new(core::sync::atomic::AtomicBool::new(false));
        let mut registered_any = false;
        for interest in &interests {
            if let Some(entry) = current_fd_entry(interest.fd)
                && fd_register_waiter(&entry, task_id, &woken)
            {
                registered_any = true;
            }
        }

        // If no pollable FDs were registered, yield once to avoid infinite blocking.
        if !registered_any {
            crate::task::yield_now();
            restore_caller_context(pid, saved_user_rsp);
            continue;
        }

        // Re-check readiness after registration (TOCTOU).
        let mut any_ready = false;
        for interest in &interests {
            if let Some(entry) = current_fd_entry(interest.fd) {
                let ready = fd_poll_events(&entry);
                let ready_u32 = ready as u16 as u32;
                if (ready_u32 & interest.events) != 0 || (ready_u32 & (EPOLLHUP | EPOLLERR)) != 0 {
                    any_ready = true;
                    break;
                }
            }
        }
        if any_ready {
            for interest in &interests {
                if let Some(entry) = current_fd_entry(interest.fd) {
                    fd_deregister_waiter(&entry, task_id);
                }
            }
            continue;
        }

        if deadline_tick.is_some() {
            // Positive timeout: yield to let timer ticks advance.
            for interest in &interests {
                if let Some(entry) = current_fd_entry(interest.fd) {
                    fd_deregister_waiter(&entry, task_id);
                }
            }
            crate::task::yield_now();
            restore_caller_context(pid, saved_user_rsp);
        } else {
            // Indefinite timeout (-1): block on wait queues.
            crate::task::scheduler::block_current_unless_woken(&woken);
            restore_caller_context(pid, saved_user_rsp);
            for interest in &interests {
                if let Some(entry) = current_fd_entry(interest.fd) {
                    fd_deregister_waiter(&entry, task_id);
                }
            }
        }
    }
}
