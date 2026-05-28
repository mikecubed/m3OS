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
    tgid: u32,
    ppid: u32,
    state: ProcessState,
    uid: u32,
    gid: u32,
    euid: u32,
    egid: u32,
    cwd: String,
    exec_path: String,
    cmdline: Vec<String>,
    comm: [u8; 16],
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
        ["meminfo" | "kmsg" | "stat" | "uptime" | "version" | "mounts" | "cpuinfo" | "loadavg"] => {
            Some(ProcfsNode::File)
        }
        ["self"] => Some(ProcfsNode::Symlink(alloc::format!(
            "/proc/{}",
            current_pid()
        ))),
        [pid] => {
            let pid = parse_pid_component(pid)?;
            process_snapshot(pid).map(|_| ProcfsNode::Dir)
        }
        [
            pid,
            "status" | "cmdline" | "comm" | "maps" | "stat" | "statm" | "io",
        ] => {
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
        // `/proc/<pid>/task/<tid>/…` — the per-thread view. htop reads the
        // main thread's stat via `task/<pid>/stat` (its `scanMainThread`
        // path), so the absence of this subtree made `readStatFile` fail
        // for every process and the table render empty. For m3OS's mostly
        // single-threaded processes `<tid>` is the pid itself; multi-thread
        // groups expose every member tid.
        [pid, "task"] => {
            let pid = parse_pid_component(pid)?;
            process_snapshot(pid).map(|_| ProcfsNode::Dir)
        }
        [pid, "task", tid] => {
            let pid = parse_pid_component(pid)?;
            let tid = parse_pid_component(tid)?;
            task_member(pid, tid).then_some(ProcfsNode::Dir)
        }
        [
            pid,
            "task",
            tid,
            "status" | "cmdline" | "comm" | "maps" | "stat" | "statm" | "io",
        ] => {
            let pid = parse_pid_component(pid)?;
            let tid = parse_pid_component(tid)?;
            task_member(pid, tid).then_some(ProcfsNode::File)
        }
        [pid, "task", tid, "fd"] => {
            let pid = parse_pid_component(pid)?;
            let tid = parse_pid_component(tid)?;
            task_member(pid, tid).then_some(ProcfsNode::Dir)
        }
        [pid, "task", tid, "exe"] => {
            let pid = parse_pid_component(pid)?;
            let tid = parse_pid_component(tid)?;
            if !task_member(pid, tid) {
                return None;
            }
            let proc = process_snapshot(tid)?;
            (!proc.exec_path.is_empty()).then_some(ProcfsNode::Symlink(proc.exec_path))
        }
        [pid, "task", tid, "fd", fd] => {
            let pid = parse_pid_component(pid)?;
            let tid = parse_pid_component(tid)?;
            if !task_member(pid, tid) {
                return None;
            }
            let fd = fd.parse::<usize>().ok()?;
            let proc = process_snapshot(tid)?;
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
    // `/proc/<pid>/comm` returns the raw stored bytes (which may not be
    // valid UTF-8) so the result aligns with what `PR_GET_NAME` would
    // return for the same process — short-circuit the String detour
    // here, see [`render_comm_bytes`].
    if let [pid, "comm"] = parts.as_slice() {
        let proc = process_snapshot(parse_pid_component(pid)?)?;
        return Some(render_comm_bytes(proc));
    }
    if let [pid, "task", tid, "comm"] = parts.as_slice() {
        let proc = task_snapshot(parse_pid_component(pid)?, parse_pid_component(tid)?)?;
        return Some(render_comm_bytes(proc));
    }
    let text = match parts.as_slice() {
        ["meminfo"] => render_meminfo(),
        ["kmsg"] => render_kmsg(),
        ["stat"] => render_stat(),
        ["uptime"] => render_uptime(),
        ["version"] => render_version(),
        ["mounts"] => render_mounts(),
        ["cpuinfo"] => render_cpuinfo(),
        ["loadavg"] => render_loadavg(),
        [pid, "status"] => render_status(process_snapshot(parse_pid_component(pid)?)?),
        [pid, "cmdline"] => render_cmdline(process_snapshot(parse_pid_component(pid)?)?),
        [pid, "maps"] => render_maps(process_snapshot(parse_pid_component(pid)?)?),
        [pid, "stat"] => render_pid_stat(process_snapshot(parse_pid_component(pid)?)?),
        [pid, "statm"] => render_pid_statm(process_snapshot(parse_pid_component(pid)?)?),
        [pid, "io"] => render_pid_io(process_snapshot(parse_pid_component(pid)?)?),
        [pid, "task", tid, "status"] => render_status(task_snapshot(
            parse_pid_component(pid)?,
            parse_pid_component(tid)?,
        )?),
        [pid, "task", tid, "cmdline"] => render_cmdline(task_snapshot(
            parse_pid_component(pid)?,
            parse_pid_component(tid)?,
        )?),
        [pid, "task", tid, "maps"] => render_maps(task_snapshot(
            parse_pid_component(pid)?,
            parse_pid_component(tid)?,
        )?),
        [pid, "task", tid, "stat"] => render_pid_stat(task_snapshot(
            parse_pid_component(pid)?,
            parse_pid_component(tid)?,
        )?),
        [pid, "task", tid, "statm"] => render_pid_statm(task_snapshot(
            parse_pid_component(pid)?,
            parse_pid_component(tid)?,
        )?),
        [pid, "task", tid, "io"] => render_pid_io(task_snapshot(
            parse_pid_component(pid)?,
            parse_pid_component(tid)?,
        )?),
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
            (String::from("cpuinfo"), false),
            (String::from("loadavg"), false),
        ];
        // Phase 72b — match Linux's default `/proc` policy: every
        // user can see every PID. Linux only hides PIDs under the
        // opt-in `hidepid=` mount option, which m3OS does not yet
        // implement. Without this, `htop` running as a non-root
        // authenticated user shows only its own process tree (itself
        // + term + ion) — the system daemons (display_server,
        // kbd_server, mouse_server, audio_server, init) are all
        // owned by root and were filtered out, making the TUI look
        // empty. The per-file readers (`process_snapshot` +
        // friends) follow the same any-user-can-read policy; the
        // sensitive content (environ, memory pages, etc.) is gated
        // by separate ACLs above this layer.
        let _ = caller_pid;
        let table = PROCESS_TABLE.lock();
        let mut pids: Vec<u32> = table.iter().map(|proc| proc.pid).collect();
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
                (String::from("comm"), false),
                (String::from("maps"), false),
                (String::from("stat"), false),
                (String::from("statm"), false),
                (String::from("io"), false),
                (String::from("exe"), false),
                (String::from("fd"), true),
                (String::from("task"), true),
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
        [pid, "task"] => {
            let pid = parse_pid_component(pid)?;
            process_snapshot(pid)?;
            let table = PROCESS_TABLE.lock();
            let mut tids: Vec<u32> = table
                .iter()
                .filter(|proc| proc.pid == pid || proc.tgid == pid)
                .map(|proc| proc.pid)
                .collect();
            drop(table);
            tids.sort_unstable();
            tids.dedup();
            Some(
                tids.into_iter()
                    .map(|t| (alloc::format!("{t}"), true))
                    .collect(),
            )
        }
        [pid, "task", tid] => {
            let pid = parse_pid_component(pid)?;
            let tid = parse_pid_component(tid)?;
            if !task_member(pid, tid) {
                return None;
            }
            Some(alloc::vec![
                (String::from("status"), false),
                (String::from("cmdline"), false),
                (String::from("comm"), false),
                (String::from("maps"), false),
                (String::from("stat"), false),
                (String::from("statm"), false),
                (String::from("io"), false),
                (String::from("exe"), false),
                (String::from("fd"), true),
            ])
        }
        [pid, "task", tid, "fd"] => {
            let pid = parse_pid_component(pid)?;
            let tid = parse_pid_component(tid)?;
            if !task_member(pid, tid) {
                return None;
            }
            let proc = process_snapshot(tid)?;
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
    let proc = table.find(pid)?;
    // Resolve the caller's euid while the table is already held so a
    // separate `PROCESS_TABLE.lock()` call doesn't deadlock with this
    // one. A `pid == 0` caller is the kernel itself; treat as root.
    let caller_pid = current_pid();
    let caller_euid = if caller_pid == 0 {
        0
    } else {
        table.find(caller_pid).map(|p| p.euid).unwrap_or(0)
    };
    // Phase 72b — match Linux's default `/proc/<pid>/*` policy: the
    // basic snapshot (pid, ppid, state, name, exec, cmdline, memory
    // sizes) is readable by anyone. Without this, `htop` running as a
    // non-root user couldn't read any system daemon's stat/status/
    // cmdline and the TUI showed an empty process list.
    //
    // Per-pid `fd_targets` is more sensitive — exposing the open-file
    // table reveals which sockets and tmpfs paths a daemon holds, an
    // information leak compared to typical Linux defaults where
    // `/proc/<pid>/fd` is `dr-x------` (owner-only). Gate it on the
    // caller's euid: only root (0) or a matching euid sees the fd
    // table; everyone else gets an empty list (the basic snapshot
    // still works for any reader, matching Linux's `/proc/<pid>/stat`
    // policy).
    let fd_targets = if caller_euid == 0 || caller_euid == proc.euid {
        let mut targets = Vec::new();
        for (fd, entry) in proc.fd_entries() {
            if let Some(target) = fd_target(&entry.backend) {
                targets.push((fd, target));
            }
        }
        targets
    } else {
        Vec::new()
    };
    Some(ProcessSnapshot {
        pid: proc.pid,
        tgid: proc.tgid,
        ppid: proc.ppid,
        state: proc.state,
        uid: proc.uid,
        gid: proc.gid,
        euid: proc.euid,
        egid: proc.egid,
        cwd: proc.cwd.clone(),
        exec_path: proc.exec_path.clone(),
        cmdline: proc.cmdline.clone(),
        comm: proc.comm,
        user_stack_top: proc.user_stack_top,
        brk_current: proc.brk_current,
        mappings: proc.vma_tree.iter().cloned().collect(),
        fd_targets,
    })
}

/// Is `tid` a thread of the process group led by `pid`?  True for the main
/// thread (`tid == pid`) and for any task whose thread-group id is `pid`.
/// Used to validate `/proc/<pid>/task/<tid>/…` paths.
fn task_member(pid: u32, tid: u32) -> bool {
    let table = PROCESS_TABLE.lock();
    match table.find(tid) {
        Some(proc) => tid == pid || proc.tgid == pid,
        None => false,
    }
}

/// Snapshot for a `/proc/<pid>/task/<tid>/…` path: the thread's own
/// snapshot, but only when `tid` actually belongs to `pid`'s thread group.
fn task_snapshot(pid: u32, tid: u32) -> Option<ProcessSnapshot> {
    if !task_member(pid, tid) {
        return None;
    }
    process_snapshot(tid)
}

/// The real process state, taken from the scheduler's canonical `TaskState`
/// rather than `Process.state` (which m3OS never moves out of `Ready`, so
/// every process otherwise reported `R (running)`).  Falls back to the
/// snapshot's stale value when no live task matches (e.g. zombies).
///
/// Must be called WITHOUT `PROCESS_TABLE` held — it acquires the scheduler
/// lock, and the two are never held simultaneously to avoid lock-order
/// inversion.
fn live_process_state(pid: u32, fallback: ProcessState) -> ProcessState {
    use crate::task::TaskState;
    match crate::task::scheduler::task_state_for_pid(pid) {
        Some(TaskState::Running) => ProcessState::Running,
        Some(TaskState::Ready) => ProcessState::Ready,
        // Any BlockedOn* variant maps to "sleeping" (S) for /proc purposes.
        Some(_) => ProcessState::Blocked,
        None => fallback,
    }
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
        FdBackend::VfsService { service_handle, .. } => {
            Some(alloc::format!("vfs:[handle={service_handle}]"))
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
    // Phase 69d follow-up: htop and other Linux tools read Buffers,
    // Cached, SwapTotal, SwapFree to drive the Mem/Swap bars. m3OS has
    // no page cache and no swap so those lines stay zero, but the
    // *presence* of the lines is what htop's parser keys off — without
    // them the Mem bar renders without a used/cached breakdown.
    alloc::format!(
        concat!(
            "MemTotal:       {:>8} kB\n",
            "MemFree:        {:>8} kB\n",
            "MemAvailable:   {:>8} kB\n",
            "Buffers:        {:>8} kB\n",
            "Cached:         {:>8} kB\n",
            "SwapCached:     {:>8} kB\n",
            "Active:         {:>8} kB\n",
            "Inactive:       {:>8} kB\n",
            "SwapTotal:      {:>8} kB\n",
            "SwapFree:       {:>8} kB\n",
            "Dirty:          {:>8} kB\n",
            "Writeback:      {:>8} kB\n",
            "Shmem:          {:>8} kB\n",
            "Slab:           {:>8} kB\n",
            "PerCpuCached:   {:>8} kB\n",
            "Allocated:      {:>8} kB\n",
            "KernelAllocator: {}\n",
            "KernelSlabPages: {:>4} kB\n",
            "KernelLargePages: {:>3} kB\n",
        ),
        total_kib,
        free_kib,
        available_kib,
        0u64,
        0u64,
        0u64,
        frames.allocated_frames * 4,
        0u64,
        0u64,
        0u64,
        0u64,
        0u64,
        0u64,
        slab_pages_kib,
        per_cpu_cached_kib,
        frames.allocated_frames * 4,
        if heap.size_class_active {
            "size-class"
        } else {
            "bootstrap"
        },
        slab_pages_kib,
        large_pages_kib,
    )
}

/// `/proc/cpuinfo` — one block per logical CPU. htop and similar tools
/// count `processor` lines to size the per-CPU stat array; without this
/// file the CPU bar count falls back to a single virtual CPU.
fn render_cpuinfo() -> String {
    let cores = crate::smp::core_count();
    let mut out = String::new();
    for core_id in 0..cores {
        let _ = writeln!(out, "processor\t: {core_id}");
        let _ = writeln!(out, "vendor_id\t: m3OS");
        let _ = writeln!(out, "cpu family\t: 6");
        let _ = writeln!(out, "model\t\t: 1");
        let _ = writeln!(out, "model name\t: m3OS virtual x86_64 core {core_id}");
        let _ = writeln!(out, "stepping\t: 1");
        let _ = writeln!(out, "cpu MHz\t\t: 1000.000");
        let _ = writeln!(out, "cache size\t: 0 KB");
        let _ = writeln!(out, "physical id\t: 0");
        let _ = writeln!(out, "siblings\t: {cores}");
        let _ = writeln!(out, "core id\t\t: {core_id}");
        let _ = writeln!(out, "cpu cores\t: {cores}");
        let _ = writeln!(out, "fpu\t\t: yes");
        let _ = writeln!(
            out,
            "flags\t\t: fpu de pse tsc msr pae mce cx8 apic sep mtrr pge mca cmov pat pse36 clflush mmx fxsr sse sse2 syscall nx rdtscp lm constant_tsc nopl xtopology nonstop_tsc cpuid pni ssse3 sse4_1 sse4_2 x2apic popcnt aes xsave avx hypervisor lahf_lm"
        );
        let _ = writeln!(out, "bogomips\t: 2000.00");
        let _ = writeln!(out, "clflush size\t: 64");
        let _ = writeln!(out, "cache_alignment\t: 64");
        let _ = writeln!(out, "address sizes\t: 48 bits physical, 48 bits virtual");
        let _ = writeln!(out);
    }
    out
}

/// `/proc/loadavg` — three load averages, running/total processes,
/// last PID. m3OS does not track real load averages; synthesise from
/// runnable process count so htop has non-zero values to render.
fn render_loadavg() -> String {
    // Total / last-pid from the process table; release it before touching
    // the scheduler (locks are never held simultaneously).
    let (total, last_pid) = {
        let table = PROCESS_TABLE.lock();
        let total = table.iter().count();
        let last_pid = table.iter().map(|p| p.pid).max().unwrap_or(0);
        (total, last_pid)
    };
    // Runnable count from the scheduler's canonical task states, NOT
    // `Process.state` (which never leaves `Ready`, so every process — even
    // daemons parked in `BlockedOnRecv` — counted as runnable and load read
    // a flat ~N).  This now reflects only genuinely runnable tasks.
    let runnable = crate::task::scheduler::runnable_task_count();
    // Format as `X.XX X.XX X.XX runnable/total last_pid`.  Using the
    // current runnable count for all three windows is a coarse but honest
    // approximation — m3OS does not yet drive the exponential moving
    // averages Linux uses.  Integer-only math keeps soft-float out of the
    // kernel; the centi fraction is always 00.
    alloc::format!("{runnable}.00 {runnable}.00 {runnable}.00 {runnable}/{total} {last_pid}\n")
}

fn render_stat() -> String {
    let btime = crate::rtc::BOOT_EPOCH_SECS.load(core::sync::atomic::Ordering::Relaxed);
    // Process count from the table; drop the lock before touching the
    // scheduler (the two locks are never held simultaneously).
    let total = {
        let table = PROCESS_TABLE.lock();
        table.iter().count()
    };
    // Real runnable count — `Process.state` is stale (always Ready), so use
    // the scheduler's canonical task states.
    let running = crate::task::scheduler::runnable_task_count();
    let cores = crate::smp::core_count() as u64;
    // Real busy/idle, derived from the per-task tick accounting the timer
    // IRQ maintains (ms, TICKS_PER_SEC = 1000). USER_HZ for the cpu* lines
    // is 100, so convert ms→jiffies by dividing by 10.  This replaces the
    // old hard-coded 10 %/90 % split that pinned htop's CPU meter at ~10 %.
    let (user_ms, system_ms, idle_ms) = crate::task::scheduler::global_cpu_times();
    let user_j = user_ms / 10;
    let system_j = system_ms / 10;
    let idle_j = idle_ms / 10;
    // Per-core lines: m3OS does not attribute non-idle busy time to a
    // specific core (tasks migrate), so distribute the aggregate evenly.
    // The bars are uniform but the percentage is real.
    let per_core = |v: u64| v.checked_div(cores).unwrap_or(v);

    let mut out = String::new();
    // Aggregate cpu line: user nice system idle iowait irq softirq steal guest guest_nice.
    let _ = writeln!(out, "cpu  {user_j} 0 {system_j} {idle_j} 0 0 0 0 0 0");
    // Per-CPU lines: htop reads these to drive its per-core CPU bars.
    for core in 0..cores {
        let _ = writeln!(
            out,
            "cpu{core} {} 0 {} {} 0 0 0 0 0 0",
            per_core(user_j),
            per_core(system_j),
            per_core(idle_j)
        );
    }
    let _ = writeln!(out, "intr 0");
    let _ = writeln!(out, "ctxt 0");
    let _ = writeln!(out, "btime {btime}");
    let _ = writeln!(out, "processes {total}");
    let _ = writeln!(out, "procs_running {running}");
    let _ = writeln!(out, "procs_blocked 0");
    out
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
    // `/tmp` and `/run` share the same tmpfs instance as distinct top-level
    // directories. See kernel/src/fs/tmpfs.rs.
    out.push_str("tmpfs /tmp tmpfs rw 0 0\n");
    out.push_str("tmpfs /run tmpfs rw 0 0\n");
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
    let state = match live_process_state(proc.pid, proc.state) {
        ProcessState::Ready | ProcessState::Running => "R (running)",
        ProcessState::Blocked => "S (sleeping)",
        ProcessState::Stopped => "T (stopped)",
        ProcessState::Zombie => "Z (zombie)",
    };
    // Phase 77 Track H.1 — emit the fields htop / ps read for their memory
    // columns. m3OS does not track a precise resident-set size, so VmRSS is
    // approximated by the total mapped size (a reasonable upper bound for this
    // simple VM model); VmStk is the fixed 64 KiB user stack; VmData sums the
    // writable anonymous/heap mappings. Tgid is the thread-group leader PID,
    // which for non-leader threads (tid != pid) differs from the thread's own
    // pid; populated from `proc.tgid` so `/proc/<pid>/task/<tid>/status` is
    // Linux-compatible.
    const STACK_KIB: u64 = (16 * 4096) / 1024; // fixed 64 KiB user stack
    let mut vm_size = 16 * 4096u64;
    let mut vm_data = 0u64;
    for mapping in &proc.mappings {
        vm_size = vm_size.saturating_add(mapping.len);
        // prot bit 0x2 == writable → counts toward the data footprint.
        if mapping.prot & 0x2 != 0 {
            vm_data = vm_data.saturating_add(mapping.len);
        }
    }
    if proc.brk_current != 0 {
        vm_data = vm_data.saturating_add(4096);
    }
    let vm_size_kib = vm_size / 1024;
    let vm_data_kib = vm_data / 1024;
    // VmRSS upper-bound: everything mapped is treated as resident in this model.
    let vm_rss_kib = vm_size_kib;
    alloc::format!(
        "Name:\t{name}\nState:\t{state}\nTgid:\t{}\nPid:\t{}\nPPid:\t{}\nUid:\t{}\t{}\t{}\t{}\nGid:\t{}\t{}\t{}\t{}\nThreads:\t1\nVmSize:\t{} kB\nVmRSS:\t{} kB\nVmData:\t{} kB\nVmStk:\t{} kB\nCwd:\t{}\n",
        proc.tgid,
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
        vm_rss_kib,
        vm_data_kib,
        STACK_KIB,
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
    let comm_visible_end = proc
        .comm
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(proc.comm.len());
    if comm_visible_end > 0 {
        // PR_SET_NAME accepts arbitrary bytes so the slice may contain
        // invalid UTF-8 (typically because the caller passed a name
        // truncated mid-multibyte sequence).  Use lossy decoding so
        // `/proc/<pid>/comm` stays observably aligned with what
        // `PR_GET_NAME` would return — falling back to cmdline/exec
        // would expose a different (possibly longer) name than the
        // stored comm bytes, defeating the point of `PR_SET_NAME`.
        return String::from_utf8_lossy(&proc.comm[..comm_visible_end]).into_owned();
    }
    if let Some(first) = proc.cmdline.first() {
        String::from(basename(first))
    } else if !proc.exec_path.is_empty() {
        String::from(basename(&proc.exec_path))
    } else {
        String::from("unknown")
    }
}

/// `/proc/<pid>/comm` — the 15-byte process name plus a trailing newline.
///
/// Returns raw bytes (not a `String`) so a `PR_SET_NAME` containing
/// invalid UTF-8 round-trips bit-for-bit through `/proc/<pid>/comm`,
/// matching `PR_GET_NAME` semantics.  An earlier implementation went
/// through `String::from_utf8_lossy`, which expanded every invalid
/// byte to a 3-byte U+FFFD replacement character and broke the
/// `name == PR_GET_NAME(PR_SET_NAME(name))` invariant
/// (PR #177 third-pass review fix).
fn render_comm_bytes(proc: ProcessSnapshot) -> Vec<u8> {
    let comm_visible_end = proc
        .comm
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(proc.comm.len());
    let mut out = if comm_visible_end > 0 {
        proc.comm[..comm_visible_end].to_vec()
    } else if let Some(first) = proc.cmdline.first() {
        basename(first).as_bytes().to_vec()
    } else if !proc.exec_path.is_empty() {
        basename(&proc.exec_path).as_bytes().to_vec()
    } else {
        b"unknown".to_vec()
    };
    out.push(b'\n');
    out
}

/// Sum of `len` across all VMAs plus the standard 16-page stack, used
/// as the process VmSize / `/proc/<pid>/stat`'s `vsize` field.
fn proc_vm_size_bytes(proc: &ProcessSnapshot) -> u64 {
    let mut vm = 16u64 * 4096;
    for mapping in &proc.mappings {
        vm = vm.saturating_add(mapping.len);
    }
    vm
}

/// `/proc/<pid>/stat` — the canonical 47-field whitespace-separated line
/// Linux exposes for tools like htop, ps, top.  The exact field count
/// (and the convention of wrapping `comm` in parentheses) is required:
/// parsers locate `state` by scanning past the trailing `)` to handle
/// `comm` values containing spaces / parens, and then index every
/// subsequent field by position.
///
/// We populate the fields we can derive from the process snapshot and
/// emit `0` for accounting fields we do not yet track (utime/stime,
/// minflt/majflt, etc.) — htop tolerates zero values and falls back to
/// drawing the row without per-process CPU%.
fn render_pid_stat(proc: ProcessSnapshot) -> String {
    let name = proc_name(&proc);
    let state_char = match live_process_state(proc.pid, proc.state) {
        ProcessState::Ready | ProcessState::Running => 'R',
        ProcessState::Blocked => 'S',
        ProcessState::Stopped => 'T',
        ProcessState::Zombie => 'Z',
    };
    let pid = proc.pid;
    let ppid = proc.ppid;
    // pgrp / session / tty_nr / tpgid: m3OS does not yet track these
    // distinctly, so report the parent's pid as a best-effort and 0
    // for TTY identifiers htop only uses to label rows.
    let pgrp = ppid;
    let session = ppid;
    let vsize = proc_vm_size_bytes(&proc);
    // rss in pages — VmSize/4096 is a generous upper bound; m3OS does
    // not page-out so RSS ~= VmSize in practice.
    let rss_pages = vsize / 4096;
    // utime / stime: the scheduler accumulates per-task CPU time in ms
    // (1 tick = 1 ms, TICKS_PER_SEC = 1000) via the timer-IRQ sampler.
    // /proc/<pid>/stat reports them in USER_HZ jiffies (100 Hz), so divide
    // by 10. Previously these were hard-coded to 0, which made htop / top /
    // ps display a flat 0% per-process CPU even for busy processes.
    let (user_ms, system_ms, _, _) = crate::task::scheduler::task_times_for_pid(pid);
    let utime = user_ms / 10;
    let stime = system_ms / 10;
    // starttime in jiffies since boot: 0 is acceptable for htop, which
    // displays this as the "TIME+" column relative to its own clock.
    let starttime = 0u64;
    // Canonical Linux `/proc/<pid>/stat` layout (`man 5 proc`).  Field
    // positions are load-bearing: htop / ps index every field by position
    // after the `comm` parenthesis, so the count before each field must be
    // exact.  Fields 1..=25 are emitted explicitly below; the remaining
    // 26..=52 (which include processor at field 39) are appended as zeros.
    //   1:pid 2:(comm) 3:state 4:ppid 5:pgrp 6:session 7:tty_nr 8:tpgid
    //   9:flags 10:minflt 11:cminflt 12:majflt 13:cmajflt 14:utime 15:stime
    //   16:cutime 17:cstime 18:priority 19:nice 20:num_threads 21:itrealvalue
    //   22:starttime 23:vsize 24:rss 25:rsslim
    let mut line = alloc::format!(
        "{pid} ({name}) {state_char} {ppid} {pgrp} {session} 0 -1 0 0 0 0 0 \
{utime} {stime} 0 0 20 0 1 0 {starttime} {vsize} {rss_pages} 18446744073709551615"
    );
    for _ in 26..=52 {
        line.push_str(" 0");
    }
    line.push('\n');
    line
}

/// `/proc/<pid>/statm` — seven decimal counts (size, resident, shared,
/// text, lib, data, dirty) measured in pages.  htop reads the `size`
/// and `resident` fields for the memory column.
fn render_pid_statm(proc: ProcessSnapshot) -> String {
    let vm = proc_vm_size_bytes(&proc);
    let pages = vm / 4096;
    // size resident shared text lib data dt
    alloc::format!("{pages} {pages} 0 0 0 {pages} 0\n")
}

/// `/proc/<pid>/io` — minimal accounting for tools that probe it.  We
/// do not track per-process byte counts yet, so every counter is zero.
fn render_pid_io(_proc: ProcessSnapshot) -> String {
    String::from(
        "rchar: 0\nwchar: 0\nsyscr: 0\nsyscw: 0\nread_bytes: 0\nwrite_bytes: 0\ncancelled_write_bytes: 0\n",
    )
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
