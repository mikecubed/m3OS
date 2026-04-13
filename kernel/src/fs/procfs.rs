//! Synthetic procfs backend (Phase 38).

extern crate alloc;

use alloc::{string::String, vec::Vec};
use core::fmt::Write;

use crate::{
    arch::x86_64::{interrupts::tick_count, syscall::TICKS_PER_SEC},
    mm::frame_allocator,
    process::{FdBackend, MemoryMapping, PROCESS_TABLE, ProcessState, current_pid},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcfsNode {
    File,
    Dir,
    Symlink(String),
}

#[derive(Clone, Copy, Debug)]
pub struct ProcfsStat {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub ino: u64,
    pub nlink: u64,
}

#[derive(Clone)]
struct ProcessSnapshot {
    pid: u32,
    ppid: u32,
    state: ProcessState,
    uid: u32,
    gid: u32,
    euid: u32,
    egid: u32,
    cwd: String,
    exec_path: String,
    cmdline: Vec<String>,
    user_stack_top: u64,
    brk_current: u64,
    mappings: Vec<MemoryMapping>,
    fd_targets: Vec<(usize, String)>,
}

pub fn path_node(abs_path: &str) -> Option<ProcfsNode> {
    let path = trim_proc_path(abs_path);
    if path == "/proc" {
        return Some(ProcfsNode::Dir);
    }

    let rel = path.strip_prefix("/proc/")?;
    let parts: Vec<&str> = rel.split('/').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        ["meminfo" | "kmsg" | "stat" | "uptime" | "version" | "mounts"] => Some(ProcfsNode::File),
        ["self"] => Some(ProcfsNode::Symlink(alloc::format!(
            "/proc/{}",
            current_pid()
        ))),
        [pid] => {
            let pid = parse_pid_component(pid)?;
            process_snapshot(pid).map(|_| ProcfsNode::Dir)
        }
        [pid, "status" | "cmdline" | "maps"] => {
            let pid = parse_pid_component(pid)?;
            process_snapshot(pid).map(|_| ProcfsNode::File)
        }
        [pid, "fd"] => {
            let pid = parse_pid_component(pid)?;
            process_snapshot(pid).map(|_| ProcfsNode::Dir)
        }
        [pid, "exe"] => {
            let pid = parse_pid_component(pid)?;
            let proc = process_snapshot(pid)?;
            (!proc.exec_path.is_empty()).then_some(ProcfsNode::Symlink(proc.exec_path))
        }
        [pid, "fd", fd] => {
            let pid = parse_pid_component(pid)?;
            let fd = fd.parse::<usize>().ok()?;
            let proc = process_snapshot(pid)?;
            proc.fd_targets
                .into_iter()
                .find(|(open_fd, _)| *open_fd == fd)
                .map(|(_, target)| ProcfsNode::Symlink(target))
        }
        _ => None,
    }
}

pub fn path_exists(abs_path: &str) -> bool {
    path_node(abs_path).is_some()
}

pub fn is_dir(abs_path: &str) -> bool {
    matches!(path_node(abs_path), Some(ProcfsNode::Dir))
}

pub fn stat(abs_path: &str) -> Option<ProcfsStat> {
    let path = trim_proc_path(abs_path);
    let node = path_node(path)?;
    let (mode, size, nlink) = match &node {
        ProcfsNode::Dir => (0x4000 | 0o555, 0, 2),
        ProcfsNode::File => {
            let size = read_file(path)?.len() as u64;
            (0x8000 | 0o444, size, 1)
        }
        ProcfsNode::Symlink(target) => (0xA000 | 0o777, target.len() as u64, 1),
    };
    Some(ProcfsStat {
        mode,
        uid: 0,
        gid: 0,
        size,
        ino: synthetic_ino(path),
        nlink,
    })
}

pub fn read_file(abs_path: &str) -> Option<Vec<u8>> {
    let path = trim_proc_path(abs_path);
    let rel = path.strip_prefix("/proc/")?;
    let parts: Vec<&str> = rel.split('/').filter(|part| !part.is_empty()).collect();
    let text = match parts.as_slice() {
        ["meminfo"] => render_meminfo(),
        ["kmsg"] => render_kmsg(),
        ["stat"] => render_stat(),
        ["uptime"] => render_uptime(),
        ["version"] => render_version(),
        ["mounts"] => render_mounts(),
        [pid, "status"] => render_status(process_snapshot(parse_pid_component(pid)?)?),
        [pid, "cmdline"] => render_cmdline(process_snapshot(parse_pid_component(pid)?)?),
        [pid, "maps"] => render_maps(process_snapshot(parse_pid_component(pid)?)?),
        _ => return None,
    };
    Some(text.into_bytes())
}

pub fn list_dir(abs_path: &str) -> Option<Vec<(String, bool)>> {
    let path = trim_proc_path(abs_path);
    if path == "/proc" {
        let caller_pid = current_pid();
        let mut entries = alloc::vec![
            (String::from("self"), false),
            (String::from("meminfo"), false),
            (String::from("kmsg"), false),
            (String::from("stat"), false),
            (String::from("uptime"), false),
            (String::from("version"), false),
            (String::from("mounts"), false),
        ];
        let table = PROCESS_TABLE.lock();
        let caller_euid = table.find(caller_pid).map(|proc| proc.euid).unwrap_or(0);
        let mut pids: Vec<u32> = table
            .iter()
            .filter(|proc| caller_euid == 0 || proc.pid == caller_pid || proc.euid == caller_euid)
            .map(|proc| proc.pid)
            .collect();
        drop(table);
        pids.sort_unstable();
        for pid in pids {
            entries.push((alloc::format!("{pid}"), true));
        }
        return Some(entries);
    }

    let rel = path.strip_prefix("/proc/")?;
    let parts: Vec<&str> = rel.split('/').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        [pid] => {
            let pid = parse_pid_component(pid)?;
            process_snapshot(pid)?;
            Some(alloc::vec![
                (String::from("status"), false),
                (String::from("cmdline"), false),
                (String::from("maps"), false),
                (String::from("exe"), false),
                (String::from("fd"), true),
            ])
        }
        [pid, "fd"] => {
            let pid = parse_pid_component(pid)?;
            let proc = process_snapshot(pid)?;
            let mut entries = Vec::new();
            for (fd, _) in proc.fd_targets {
                entries.push((alloc::format!("{fd}"), false));
            }
            Some(entries)
        }
        _ => None,
    }
}

fn trim_proc_path(path: &str) -> &str {
    if path == "/proc" {
        path
    } else {
        path.trim_end_matches('/')
    }
}

fn parse_pid_component(component: &str) -> Option<u32> {
    if component == "self" {
        Some(current_pid())
    } else {
        component.parse::<u32>().ok()
    }
}

fn process_snapshot(pid: u32) -> Option<ProcessSnapshot> {
    let table = PROCESS_TABLE.lock();
    let caller_pid = current_pid();
    let caller_euid = table.find(caller_pid).map(|proc| proc.euid).unwrap_or(0);
    let proc = table.find(pid)?;
    if caller_euid != 0 && proc.pid != caller_pid && proc.euid != caller_euid {
        return None;
    }
    let mut fd_targets = Vec::new();
    for (fd, entry) in proc.fd_entries() {
        if let Some(target) = fd_target(&entry.backend) {
            fd_targets.push((fd, target));
        }
    }
    Some(ProcessSnapshot {
        pid: proc.pid,
        ppid: proc.ppid,
        state: proc.state,
        uid: proc.uid,
        gid: proc.gid,
        euid: proc.euid,
        egid: proc.egid,
        cwd: proc.cwd.clone(),
        exec_path: proc.exec_path.clone(),
        cmdline: proc.cmdline.clone(),
        user_stack_top: proc.user_stack_top,
        brk_current: proc.brk_current,
        mappings: proc.vma_tree.iter().cloned().collect(),
        fd_targets,
    })
}

fn fd_target(backend: &FdBackend) -> Option<String> {
    match backend {
        FdBackend::Stdin | FdBackend::Stdout | FdBackend::DeviceTTY { .. } => {
            Some(String::from("/dev/tty"))
        }
        FdBackend::Ramdisk { .. } => Some(String::from("ramdisk:[static]")),
        FdBackend::Tmpfs { path } => Some(if path.is_empty() {
            String::from("/tmp")
        } else {
            alloc::format!("/tmp/{path}")
        }),
        FdBackend::Fat32Disk { path, .. } => Some(if path.is_empty() {
            String::from("/data")
        } else {
            alloc::format!("/data/{path}")
        }),
        FdBackend::Ext2Disk { path, .. } => Some(if path.is_empty() {
            String::from("/")
        } else {
            alloc::format!("/{path}")
        }),
        FdBackend::PipeRead { pipe_id } | FdBackend::PipeWrite { pipe_id } => {
            Some(alloc::format!("pipe:[{pipe_id}]"))
        }
        FdBackend::Dir { path } => Some(path.clone()),
        FdBackend::DevNull => Some(String::from("/dev/null")),
        FdBackend::DevZero => Some(String::from("/dev/zero")),
        FdBackend::DevUrandom => Some(String::from("/dev/urandom")),
        FdBackend::DevFull => Some(String::from("/dev/full")),
        FdBackend::Proc { path, .. } => Some(path.clone()),
        FdBackend::PtyMaster { pty_id } => Some(alloc::format!("/dev/ptmx:{pty_id}")),
        FdBackend::PtySlave { pty_id } => Some(alloc::format!("/dev/pts/{pty_id}")),
        FdBackend::Socket { handle } => Some(alloc::format!("socket:[{handle}]")),
        FdBackend::UnixSocket { handle } => Some(alloc::format!("unix:[{handle}]")),
        FdBackend::Epoll { instance_id } => {
            Some(alloc::format!("anon_inode:[eventpoll:{instance_id}]"))
        }
    }
}

fn render_meminfo() -> String {
    let frames = frame_allocator::frame_stats();
    let heap = crate::mm::heap::heap_stats();
    let total_kib = frames.total_frames * 4;
    // MemFree: buddy-managed only (excludes per-CPU caches).
    let free_kib = frames.free_frames * 4;
    // MemAvailable: buddy free + reclaimable per-CPU caches.
    let available_kib = frames.available_frames * 4;
    let per_cpu_cached_kib = frames.per_cpu_cached * 4;
    let slab_pages_kib = heap.slab_pages * 4;
    let large_pages_kib = heap.page_backed_pages * 4;
    alloc::format!(
        concat!(
            "MemTotal:     {:>8} kB\n",
            "MemFree:      {:>8} kB\n",
            "MemAvailable: {:>8} kB\n",
            "PerCpuCached: {:>8} kB\n",
            "Allocated:    {:>8} kB\n",
            "KernelAllocator: {}\n",
            "KernelSlabPages: {:>4} kB\n",
            "KernelLargePages: {:>3} kB\n"
        ),
        total_kib,
        free_kib,
        available_kib,
        per_cpu_cached_kib,
        frames.allocated_frames * 4,
        if heap.size_class_active {
            "size-class"
        } else {
            "bootstrap"
        },
        slab_pages_kib,
        large_pages_kib
    )
}

fn render_stat() -> String {
    let ticks = tick_count();
    let btime = crate::rtc::BOOT_EPOCH_SECS.load(core::sync::atomic::Ordering::Relaxed);
    let table = PROCESS_TABLE.lock();
    let total = table.iter().count();
    let running = table
        .iter()
        .filter(|proc| proc.state == ProcessState::Running)
        .count();
    drop(table);
    alloc::format!(
        "cpu  {ticks} 0 0 {} 0 0 0 0 0 0\nbtime {btime}\nprocesses {total}\nprocs_running {running}\n",
        ticks.saturating_mul(8)
    )
}

fn render_uptime() -> String {
    let ticks = tick_count();
    let secs = ticks / TICKS_PER_SEC;
    let centis = (ticks % TICKS_PER_SEC) * 100 / TICKS_PER_SEC;
    alloc::format!("{secs}.{centis:02} {secs}.{centis:02}\n")
}

fn render_version() -> String {
    alloc::format!("m3OS version {}\n", env!("CARGO_PKG_VERSION"))
}

fn render_mounts() -> String {
    let mut out = String::new();
    let root_fs = if crate::fs::ext2::is_mounted() {
        "rootfs / ext2 rw 0 0\n"
    } else {
        "rootfs / ramfs ro 0 0\n"
    };
    out.push_str(root_fs);
    out.push_str("proc /proc proc rw 0 0\n");
    out.push_str("tmpfs /tmp tmpfs rw 0 0\n");
    out.push_str("dev /dev ramfs rw 0 0\n");
    if crate::fs::fat32::is_mounted() {
        out.push_str("/dev/vda1 /data vfat rw 0 0\n");
    }
    out
}

fn render_kmsg() -> String {
    String::from_utf8_lossy(&crate::serial::dmesg_snapshot()).into_owned()
}

pub fn render_kmsg_bytes() -> Vec<u8> {
    crate::serial::dmesg_snapshot()
}

fn render_status(proc: ProcessSnapshot) -> String {
    let name = proc_name(&proc);
    let state = match proc.state {
        ProcessState::Ready | ProcessState::Running => "R (running)",
        ProcessState::Blocked => "S (sleeping)",
        ProcessState::Stopped => "T (stopped)",
        ProcessState::Zombie => "Z (zombie)",
    };
    let mut vm_size = 16 * 4096u64;
    for mapping in &proc.mappings {
        vm_size = vm_size.saturating_add(mapping.len);
    }
    let vm_size_kib = vm_size / 1024;
    alloc::format!(
        "Name:\t{name}\nState:\t{state}\nPid:\t{}\nPPid:\t{}\nUid:\t{}\t{}\t{}\t{}\nGid:\t{}\t{}\t{}\t{}\nThreads:\t1\nVmSize:\t{} kB\nCwd:\t{}\n",
        proc.pid,
        proc.ppid,
        proc.uid,
        proc.euid,
        proc.euid,
        proc.euid,
        proc.gid,
        proc.egid,
        proc.egid,
        proc.egid,
        vm_size_kib,
        proc.cwd
    )
}

fn render_cmdline(proc: ProcessSnapshot) -> String {
    let mut out = Vec::new();
    for arg in proc.cmdline {
        out.extend_from_slice(arg.as_bytes());
        out.push(0);
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn render_maps(proc: ProcessSnapshot) -> String {
    let mut out = String::new();
    let stack_top = proc.user_stack_top;
    let stack_start = stack_top.saturating_sub(16 * 4096) & !0xfff;
    let _ = writeln!(
        out,
        "{stack_start:016x}-{stack_top:016x} rw-p 00000000 00:00 0 [stack]"
    );
    for mapping in proc.mappings {
        let start = mapping.start;
        let end = mapping.start.saturating_add(mapping.len);
        let perms = mapping_perms(&mapping);
        let _ = writeln!(
            out,
            "{start:016x}-{end:016x} {perms} 00000000 00:00 0 [anon]"
        );
    }
    if proc.brk_current != 0 {
        let heap_end = proc.brk_current;
        let heap_start = heap_end.saturating_sub(4096) & !0xfff;
        let _ = writeln!(
            out,
            "{heap_start:016x}-{heap_end:016x} rw-p 00000000 00:00 0 [heap]"
        );
    }
    out
}

fn mapping_perms(mapping: &MemoryMapping) -> String {
    let chars = [
        if mapping.prot & 0x1 != 0 { 'r' } else { '-' },
        if mapping.prot & 0x2 != 0 { 'w' } else { '-' },
        if mapping.prot & 0x4 != 0 { 'x' } else { '-' },
        'p',
    ];
    chars.iter().collect()
}

fn proc_name(proc: &ProcessSnapshot) -> String {
    if let Some(first) = proc.cmdline.first() {
        String::from(basename(first))
    } else if !proc.exec_path.is_empty() {
        String::from(basename(&proc.exec_path))
    } else {
        String::from("unknown")
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn synthetic_ino(path: &str) -> u64 {
    let mut hash = 1469598103934665603u64;
    for byte in path.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}
