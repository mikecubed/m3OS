//! m3OS init — PID 1 data-driven service manager (Phase 46).
//!
//! Responsibilities:
//! - Mount ext2 root filesystem at /
//! - Parse service definitions from `/etc/services.d/*.conf`
//! - Build dependency graph, topological start order
//! - Fork+exec services respecting dependencies
//! - Reap children, auto-restart per policy
//! - Handle SIGTERM for orderly shutdown
//! - Spawn login session separately (not a managed service)
//! - Accept control commands via `/var/run/init.cmd`
//! - Write service status to `/var/run/services.status`
//! - Never exit (kernel panics if PID 1 dies)
#![no_std]
#![no_main]

use syscall_lib::{
    AF_UNIX, O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, SOCK_DGRAM, STDOUT_FILENO, SigAction,
    SockaddrUn, WNOHANG, clock_gettime, close, execve, exit, fork, getdents64, kill, mount,
    nanosleep, open, read, rt_sigaction, sendto_unix, socket, waitpid, write, write_str, write_u64,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MAX_SERVICES: usize = 16;
const MAX_PIDS: usize = 64;
const MAX_DEPS: usize = 4;
const MAX_NAME: usize = 32;
const MAX_CMD: usize = 64;
const BUF_SIZE: usize = 512;

const SIGTERM: i32 = syscall_lib::SIGTERM;
const SIGKILL: i32 = syscall_lib::SIGKILL;

const LOGIN_PATH: &[u8] = b"/bin/login\0";
const LOGIN_ARGV0: &[u8] = b"/bin/login\0";
const ENV_PATH: &[u8] = b"PATH=/bin:/sbin:/usr/bin\0";
const ENV_HOME: &[u8] = b"HOME=/\0";
const ENV_TERM: &[u8] = b"TERM=m3os\0";
const ENV_EDITOR: &[u8] = b"EDITOR=/bin/edit\0";

const STATUS_FILE: &[u8] = b"/var/run/services.status\0";
const CMD_FILE: &[u8] = b"/var/run/init.cmd\0";

/// Known service config files to try opening (no readdir available).
const KNOWN_CONFIGS: &[&[u8]] = &[
    b"/etc/services.d/sshd.conf\0",
    b"/etc/services.d/telnetd.conf\0",
    b"/etc/services.d/syslogd.conf\0",
    b"/etc/services.d/crond.conf\0",
    b"/etc/services.d/httpd.conf\0",
    b"/etc/services.d/dhcpd.conf\0",
    b"/etc/services.d/ntpd.conf\0",
    b"/etc/services.d/ftpd.conf\0",
];

// ---------------------------------------------------------------------------
// Service types and status
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum ServiceType {
    Daemon,
    Oneshot,
}

#[derive(Clone, Copy, PartialEq)]
enum RestartPolicy {
    Always,
    OnFailure,
    Never,
}

/// Service lifecycle state machine.
///
/// Valid transitions:
///   NeverStarted ──→ Starting
///   Starting     ──→ Running
///   Starting     ──→ Stopped (exec failed)
///   Running      ──→ Stopping
///   Running      ──→ Stopped (unexpected exit)
///   Stopping     ──→ Stopped
///   Stopped      ──→ Starting (restart)
///   Stopped      ──→ PermanentlyStopped (max restarts exceeded)
///   *            ──→ PermanentlyStopped (unresolvable deps, etc.)
///   PermanentlyStopped ──→ (nothing — terminal state)
#[derive(Clone, Copy, PartialEq)]
enum ServiceStatus {
    NeverStarted,
    Starting,
    Running,
    Stopping,
    Stopped(i32),
    PermanentlyStopped,
}

impl ServiceStatus {
    /// Validate whether a transition from `self` to `target` is valid.
    fn try_transition(&self, target: ServiceStatus) -> bool {
        match (*self, target) {
            // Terminal state.
            (ServiceStatus::PermanentlyStopped, _) => false,
            // NeverStarted → Starting or PermanentlyStopped.
            (ServiceStatus::NeverStarted, ServiceStatus::Starting) => true,
            (ServiceStatus::NeverStarted, ServiceStatus::PermanentlyStopped) => true,
            (ServiceStatus::NeverStarted, _) => false,
            // Starting → Running, Stopped, or PermanentlyStopped.
            (ServiceStatus::Starting, ServiceStatus::Running) => true,
            (ServiceStatus::Starting, ServiceStatus::Stopped(_)) => true,
            (ServiceStatus::Starting, ServiceStatus::PermanentlyStopped) => true,
            (ServiceStatus::Starting, _) => false,
            // Running → Stopping, Stopped, or PermanentlyStopped.
            (ServiceStatus::Running, ServiceStatus::Stopping) => true,
            (ServiceStatus::Running, ServiceStatus::Stopped(_)) => true,
            (ServiceStatus::Running, ServiceStatus::PermanentlyStopped) => true,
            (ServiceStatus::Running, _) => false,
            // Stopping → Stopped or PermanentlyStopped.
            (ServiceStatus::Stopping, ServiceStatus::Stopped(_)) => true,
            (ServiceStatus::Stopping, ServiceStatus::PermanentlyStopped) => true,
            (ServiceStatus::Stopping, _) => false,
            // Stopped → Starting or PermanentlyStopped.
            (ServiceStatus::Stopped(_), ServiceStatus::Starting) => true,
            (ServiceStatus::Stopped(_), ServiceStatus::PermanentlyStopped) => true,
            (ServiceStatus::Stopped(_), _) => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Fixed-size string helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct FixedStr<const N: usize> {
    data: [u8; N],
    len: usize,
}

impl<const N: usize> FixedStr<N> {
    const fn new() -> Self {
        Self {
            data: [0u8; N],
            len: 0,
        }
    }

    fn from_bytes(src: &[u8]) -> Self {
        let mut s = Self::new();
        let copy_len = if src.len() < N { src.len() } else { N };
        let mut i = 0;
        while i < copy_len {
            s.data[i] = src[i];
            i += 1;
        }
        s.len = copy_len;
        s
    }

    fn as_bytes(&self) -> &[u8] {
        &self.data[..self.len]
    }

    fn eq_bytes(&self, other: &[u8]) -> bool {
        if self.len != other.len() {
            return false;
        }
        let mut i = 0;
        while i < self.len {
            if self.data[i] != other[i] {
                return false;
            }
            i += 1;
        }
        true
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Write into a buffer with null terminator, return total length including null.
    fn write_null_terminated(&self, dst: &mut [u8]) -> usize {
        let copy_len = if self.len < dst.len() - 1 {
            self.len
        } else {
            dst.len() - 1
        };
        let mut i = 0;
        while i < copy_len {
            dst[i] = self.data[i];
            i += 1;
        }
        dst[i] = 0;
        copy_len + 1
    }
}

// ---------------------------------------------------------------------------
// ServiceDef
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct ServiceDef {
    name: FixedStr<MAX_NAME>,
    command: FixedStr<MAX_CMD>,
    service_type: ServiceType,
    restart_policy: RestartPolicy,
    max_restart: u32,
    deps: [FixedStr<MAX_NAME>; MAX_DEPS],
    dep_count: usize,
    /// Current status.
    status: ServiceStatus,
    /// PID when running (0 if not running).
    pid: i32,
    /// Number of restarts performed.
    restart_count: u32,
    /// Epoch seconds when the service was last started (for backoff reset).
    last_start_time: u64,
    /// Epoch seconds when the last state transition occurred.
    last_change_time: u64,
    /// Whether this service is active (slot in use).
    active: bool,
    /// UID to run the service as (0 = root, default).
    run_as_uid: u32,
    /// Seconds to wait after SIGTERM before SIGKILL during shutdown (default 5).
    stop_timeout: u32,
}

impl ServiceDef {
    const fn empty() -> Self {
        Self {
            name: FixedStr::new(),
            command: FixedStr::new(),
            service_type: ServiceType::Daemon,
            restart_policy: RestartPolicy::Never,
            max_restart: 10,
            deps: [FixedStr::new(); MAX_DEPS],
            dep_count: 0,
            status: ServiceStatus::NeverStarted,
            pid: 0,
            restart_count: 0,
            last_start_time: 0,
            last_change_time: 0,
            active: false,
            run_as_uid: 0,
            stop_timeout: 5,
        }
    }
}

// ---------------------------------------------------------------------------
// PidTable: maps PIDs to service indices
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct PidEntry {
    pid: i32,
    service_idx: usize,
}

struct PidTable {
    entries: [PidEntry; MAX_PIDS],
    count: usize,
}

impl PidTable {
    const fn new() -> Self {
        const EMPTY: PidEntry = PidEntry {
            pid: 0,
            service_idx: 0,
        };
        Self {
            entries: [EMPTY; MAX_PIDS],
            count: 0,
        }
    }

    fn insert(&mut self, pid: i32, idx: usize) {
        if self.count < MAX_PIDS {
            self.entries[self.count] = PidEntry {
                pid,
                service_idx: idx,
            };
            self.count += 1;
        }
    }

    fn lookup(&self, pid: i32) -> Option<usize> {
        let mut i = 0;
        while i < self.count {
            if self.entries[i].pid == pid {
                return Some(self.entries[i].service_idx);
            }
            i += 1;
        }
        None
    }

    fn remove(&mut self, pid: i32) {
        let mut i = 0;
        while i < self.count {
            if self.entries[i].pid == pid {
                // Swap-remove.
                self.count -= 1;
                self.entries[i] = self.entries[self.count];
                return;
            }
            i += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Dependency Graph
// ---------------------------------------------------------------------------

struct DepGraph {
    /// For each service i, indices of services it depends on.
    depends: [[usize; MAX_DEPS]; MAX_SERVICES],
    depends_count: [usize; MAX_SERVICES],
    /// For each service i, indices of services that depend on it (reverse edges).
    required_by: [[usize; MAX_DEPS]; MAX_SERVICES],
    required_by_count: [usize; MAX_SERVICES],
}

impl DepGraph {
    const fn new() -> Self {
        Self {
            depends: [[0; MAX_DEPS]; MAX_SERVICES],
            depends_count: [0; MAX_SERVICES],
            required_by: [[0; MAX_DEPS]; MAX_SERVICES],
            required_by_count: [0; MAX_SERVICES],
        }
    }

    /// Build the dependency graph. Returns a list of service indices whose
    /// dependencies could not be resolved (they should be marked PermanentlyStopped).
    fn build(services: &[ServiceDef; MAX_SERVICES], count: usize) -> (Self, [bool; MAX_SERVICES]) {
        let mut g = Self::new();
        let mut unresolvable = [false; MAX_SERVICES];

        // Build forward edges.
        let mut i = 0;
        while i < count {
            if !services[i].active {
                i += 1;
                continue;
            }
            let mut d = 0;
            while d < services[i].dep_count {
                // Find dep index by name.
                let dep_name = &services[i].deps[d];
                let mut j = 0;
                let mut found = false;
                while j < count {
                    if j != i
                        && services[j].active
                        && services[j].name.eq_bytes(dep_name.as_bytes())
                    {
                        if g.depends_count[i] < MAX_DEPS {
                            g.depends[i][g.depends_count[i]] = j;
                            g.depends_count[i] += 1;
                        }
                        // Build reverse edge.
                        if g.required_by_count[j] < MAX_DEPS {
                            g.required_by[j][g.required_by_count[j]] = i;
                            g.required_by_count[j] += 1;
                        }
                        found = true;
                        break;
                    }
                    j += 1;
                }
                if !found {
                    // Unresolvable dependency — log warning.
                    write_str(STDOUT_FILENO, "init: warning: service '");
                    write(STDOUT_FILENO, services[i].name.as_bytes());
                    write_str(STDOUT_FILENO, "' has unresolvable dep '");
                    write(STDOUT_FILENO, dep_name.as_bytes());
                    write_str(STDOUT_FILENO, "'\n");
                    unresolvable[i] = true;
                }
                d += 1;
            }
            i += 1;
        }
        (g, unresolvable)
    }

    /// Check for cycles using DFS with a visited set. Returns true if cycle found.
    fn has_cycle(&self, count: usize) -> bool {
        // 0=unvisited, 1=in-progress, 2=done
        let mut state = [0u8; MAX_SERVICES];
        let mut i = 0;
        while i < count {
            if state[i] == 0 && self.dfs_cycle(i, count, &mut state) {
                return true;
            }
            i += 1;
        }
        false
    }

    fn dfs_cycle(&self, node: usize, count: usize, state: &mut [u8; MAX_SERVICES]) -> bool {
        state[node] = 1;
        let mut d = 0;
        while d < self.depends_count[node] {
            let dep = self.depends[node][d];
            if dep < count {
                if state[dep] == 1 {
                    return true; // back edge = cycle
                }
                if state[dep] == 0 && self.dfs_cycle(dep, count, state) {
                    return true;
                }
            }
            d += 1;
        }
        state[node] = 2;
        false
    }

    /// Produce a topological start order. Returns ordered indices and count.
    fn topo_order(
        &self,
        services: &[ServiceDef; MAX_SERVICES],
        count: usize,
    ) -> ([usize; MAX_SERVICES], usize) {
        let mut order = [0usize; MAX_SERVICES];
        let mut order_len = 0;
        let mut visited = [false; MAX_SERVICES];

        let mut i = 0;
        while i < count {
            if !visited[i] && services[i].active {
                self.topo_visit(i, count, services, &mut visited, &mut order, &mut order_len);
            }
            i += 1;
        }
        (order, order_len)
    }

    fn topo_visit(
        &self,
        node: usize,
        count: usize,
        services: &[ServiceDef; MAX_SERVICES],
        visited: &mut [bool; MAX_SERVICES],
        order: &mut [usize; MAX_SERVICES],
        order_len: &mut usize,
    ) {
        if visited[node] || !services[node].active {
            return;
        }
        visited[node] = true;
        // Visit dependencies first.
        let mut d = 0;
        while d < self.depends_count[node] {
            let dep = self.depends[node][d];
            if dep < count {
                self.topo_visit(dep, count, services, visited, order, order_len);
            }
            d += 1;
        }
        if *order_len < MAX_SERVICES {
            order[*order_len] = node;
            *order_len += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Parser: key=value service definition
// ---------------------------------------------------------------------------

/// Parse a service definition from a buffer of `key=value` lines.
fn parse_service_def(buf: &[u8], len: usize) -> Option<ServiceDef> {
    let mut svc = ServiceDef::empty();
    let mut pos = 0;

    while pos < len {
        // Skip whitespace and blank lines.
        while pos < len && (buf[pos] == b' ' || buf[pos] == b'\t' || buf[pos] == b'\r') {
            pos += 1;
        }
        if pos >= len {
            break;
        }
        // Skip comment lines.
        if buf[pos] == b'#' {
            while pos < len && buf[pos] != b'\n' {
                pos += 1;
            }
            if pos < len {
                pos += 1;
            }
            continue;
        }
        if buf[pos] == b'\n' {
            pos += 1;
            continue;
        }

        // Find '='.
        let line_start = pos;
        let mut eq_pos = pos;
        while eq_pos < len && buf[eq_pos] != b'=' && buf[eq_pos] != b'\n' {
            eq_pos += 1;
        }
        if eq_pos >= len || buf[eq_pos] != b'=' {
            // Malformed line, skip.
            while pos < len && buf[pos] != b'\n' {
                pos += 1;
            }
            if pos < len {
                pos += 1;
            }
            continue;
        }

        let key = &buf[line_start..eq_pos];
        let val_start = eq_pos + 1;
        let mut val_end = val_start;
        while val_end < len && buf[val_end] != b'\n' && buf[val_end] != b'\r' {
            val_end += 1;
        }
        let val = &buf[val_start..val_end];

        // Trim trailing whitespace from value.
        let mut val_trimmed_end = val.len();
        while val_trimmed_end > 0
            && (val[val_trimmed_end - 1] == b' ' || val[val_trimmed_end - 1] == b'\t')
        {
            val_trimmed_end -= 1;
        }
        let val = &val[..val_trimmed_end];

        if bytes_eq(key, b"name") {
            svc.name = FixedStr::from_bytes(val);
        } else if bytes_eq(key, b"command") {
            svc.command = FixedStr::from_bytes(val);
        } else if bytes_eq(key, b"type") {
            if bytes_eq(val, b"oneshot") {
                svc.service_type = ServiceType::Oneshot;
            } else {
                svc.service_type = ServiceType::Daemon;
            }
        } else if bytes_eq(key, b"restart") {
            if bytes_eq(val, b"always") {
                svc.restart_policy = RestartPolicy::Always;
            } else if bytes_eq(val, b"on-failure") {
                svc.restart_policy = RestartPolicy::OnFailure;
            } else {
                svc.restart_policy = RestartPolicy::Never;
            }
        } else if bytes_eq(key, b"max_restart") {
            svc.max_restart = parse_u32(val);
        } else if bytes_eq(key, b"depends") {
            // Comma-separated list of dependency names.
            parse_deps(val, &mut svc.deps, &mut svc.dep_count);
        } else if bytes_eq(key, b"user") {
            svc.run_as_uid = parse_u32(val);
        } else if bytes_eq(key, b"stop_timeout") {
            let v = parse_u32(val);
            if v > 0 {
                svc.stop_timeout = v;
            }
        } else {
            // Unknown field — log warning.
            write_str(STDOUT_FILENO, "init: warning: unknown field '");
            write(STDOUT_FILENO, key);
            write_str(STDOUT_FILENO, "' in service config\n");
        }

        // Advance past end of line.
        pos = val_end;
        if pos < len && buf[pos] == b'\r' {
            pos += 1;
        }
        if pos < len && buf[pos] == b'\n' {
            pos += 1;
        }
    }

    if svc.name.is_empty() || svc.command.is_empty() {
        return None;
    }
    svc.active = true;
    Some(svc)
}

fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

fn parse_u32(val: &[u8]) -> u32 {
    let mut result: u32 = 0;
    let mut i = 0;
    while i < val.len() {
        if val[i] >= b'0' && val[i] <= b'9' {
            result = match result
                .checked_mul(10)
                .and_then(|v| v.checked_add((val[i] - b'0') as u32))
            {
                Some(v) => v,
                None => return 0, // overflow → default
            };
        } else {
            return 0; // non-digit → default
        }
        i += 1;
    }
    result
}

fn parse_deps(val: &[u8], deps: &mut [FixedStr<MAX_NAME>; MAX_DEPS], count: &mut usize) {
    *count = 0;
    let mut pos = 0;
    while pos < val.len() && *count < MAX_DEPS {
        // Skip leading whitespace and commas.
        while pos < val.len() && (val[pos] == b' ' || val[pos] == b',' || val[pos] == b'\t') {
            pos += 1;
        }
        if pos >= val.len() {
            break;
        }
        let start = pos;
        while pos < val.len() && val[pos] != b',' && val[pos] != b' ' && val[pos] != b'\t' {
            pos += 1;
        }
        if pos > start {
            deps[*count] = FixedStr::from_bytes(&val[start..pos]);
            *count += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Service manager state
// ---------------------------------------------------------------------------

struct ServiceManager {
    services: [ServiceDef; MAX_SERVICES],
    count: usize,
    pid_table: PidTable,
    shutdown_requested: bool,
    login_pid: i32,
    /// File descriptor for the syslog socket (`/dev/log`), or -1 if unavailable.
    syslog_fd: i32,
}

/// Syslog severity levels.
#[allow(dead_code)]
const SEV_ERROR: u8 = 3;
#[allow(dead_code)]
const SEV_WARNING: u8 = 4;
#[allow(dead_code)]
const SEV_INFO: u8 = 6;

/// Syslog facility: daemon = 3.
#[allow(dead_code)]
const FACILITY_DAEMON: u8 = 3;

impl ServiceManager {
    const fn new() -> Self {
        Self {
            services: [ServiceDef::empty(); MAX_SERVICES],
            count: 0,
            pid_table: PidTable::new(),
            shutdown_requested: false,
            login_pid: -1,
            syslog_fd: -1,
        }
    }

    /// Open a DGRAM socket to `/dev/log` for syslog output.
    fn open_syslog(&mut self) {
        let fd = socket(AF_UNIX as i32, SOCK_DGRAM as i32, 0);
        if fd < 0 {
            return;
        }
        self.syslog_fd = fd as i32;
    }

    /// Send a log message through syslog (if available) and always echo to serial.
    #[allow(dead_code)]
    fn log_to_syslog(&self, severity: u8, msg: &[u8]) {
        // Always echo to stdout/serial.
        write_str(STDOUT_FILENO, "init: ");
        write(STDOUT_FILENO, msg);
        write_str(STDOUT_FILENO, "\n");

        // Send to syslog if available.
        if self.syslog_fd >= 0 {
            let priority = ((FACILITY_DAEMON as u32) << 3) | (severity as u32);
            let mut buf = [0u8; 256];
            let mut pos = 0;
            // Format: <priority>init: message
            buf[pos] = b'<';
            pos += 1;
            pos += write_u32_to_buf(&mut buf[pos..], priority);
            buf[pos] = b'>';
            pos += 1;
            let tag = b"init: ";
            let tag_len = tag.len().min(buf.len() - pos);
            buf[pos..pos + tag_len].copy_from_slice(&tag[..tag_len]);
            pos += tag_len;
            let msg_len = msg.len().min(buf.len() - pos);
            buf[pos..pos + msg_len].copy_from_slice(&msg[..msg_len]);
            pos += msg_len;

            let addr = SockaddrUn::new("/dev/log");
            let _ = sendto_unix(self.syslog_fd, &buf[..pos], 0, &addr);
        }
    }

    /// Load service definitions from `/etc/services.d/`.
    ///
    /// Tries to scan the directory first using `getdents64`. If the directory
    /// cannot be opened, falls back to the hardcoded `KNOWN_CONFIGS` list.
    fn load_services(&mut self) {
        let dir_fd = open(b"/etc/services.d\0", O_RDONLY, 0);
        if dir_fd >= 0 {
            self.load_services_from_dir(dir_fd as i32);
            close(dir_fd as i32);
        } else {
            // Fallback: try hardcoded config paths.
            self.load_services_from_known_configs();
        }

        if self.count == 0 {
            write_str(
                STDOUT_FILENO,
                "init: no service configs found, using built-in defaults\n",
            );
            self.add_builtin_defaults();
        }
    }

    /// Scan `/etc/services.d/` directory using getdents64 and load `.conf` files.
    fn load_services_from_dir(&mut self, dir_fd: i32) {
        let mut dent_buf = [0u8; 1024];
        loop {
            if self.count >= MAX_SERVICES {
                break;
            }
            let n = getdents64(dir_fd, &mut dent_buf);
            if n <= 0 {
                break;
            }
            let n = n as usize;
            let mut pos = 0;
            while pos < n && self.count < MAX_SERVICES {
                // Each dirent64: u64 ino, u64 off, u16 reclen, u8 type, name[]
                if pos + 19 > n {
                    break;
                }
                let reclen = (dent_buf[pos + 16] as usize) | ((dent_buf[pos + 17] as usize) << 8);
                if reclen == 0 || pos + reclen > n {
                    break;
                }
                // Name starts at offset 19.
                let name_start = pos + 19;
                let name_end = pos + reclen;
                // Find null terminator.
                let mut name_len = 0;
                while name_start + name_len < name_end && dent_buf[name_start + name_len] != 0 {
                    name_len += 1;
                }
                let name = &dent_buf[name_start..name_start + name_len];

                // Filter for .conf files.
                if name_len > 5 && name[name_len - 5..] == *b".conf" {
                    // Build full path: /etc/services.d/<name>\0
                    let prefix = b"/etc/services.d/";
                    let path_len = prefix.len() + name_len + 1; // +1 for null
                    if path_len <= BUF_SIZE {
                        let mut path_buf = [0u8; BUF_SIZE];
                        let mut pi = 0;
                        while pi < prefix.len() {
                            path_buf[pi] = prefix[pi];
                            pi += 1;
                        }
                        let mut ni = 0;
                        while ni < name_len {
                            path_buf[pi] = name[ni];
                            pi += 1;
                            ni += 1;
                        }
                        path_buf[pi] = 0; // null terminate

                        self.try_load_config(&path_buf[..pi + 1]);
                    }
                }

                pos += reclen;
            }
        }
    }

    /// Fallback: iterate over hardcoded config paths.
    fn load_services_from_known_configs(&mut self) {
        let mut i = 0;
        while i < KNOWN_CONFIGS.len() {
            if self.count >= MAX_SERVICES {
                break;
            }
            self.try_load_config(KNOWN_CONFIGS[i]);
            i += 1;
        }
    }

    /// Try to open, read, and parse a single service config file.
    fn try_load_config(&mut self, path: &[u8]) {
        let fd = open(path, O_RDONLY, 0);
        if fd >= 0 {
            let mut buf = [0u8; BUF_SIZE];
            let n = read(fd as i32, &mut buf);
            close(fd as i32);
            if n > 0 {
                match parse_service_def(&buf, n as usize) {
                    Some(svc) => {
                        write_str(STDOUT_FILENO, "init: loaded service '");
                        write(STDOUT_FILENO, svc.name.as_bytes());
                        write_str(STDOUT_FILENO, "'\n");
                        self.services[self.count] = svc;
                        self.count += 1;
                    }
                    None => {
                        write_str(STDOUT_FILENO, "init: warning: malformed service file ");
                        write(STDOUT_FILENO, path);
                        write_str(STDOUT_FILENO, "\n");
                    }
                }
            }
        }
    }

    /// Fallback: register built-in service definitions for telnetd and sshd.
    fn add_builtin_defaults(&mut self) {
        // telnetd
        if self.count < MAX_SERVICES {
            let mut svc = ServiceDef::empty();
            svc.name = FixedStr::from_bytes(b"telnetd");
            svc.command = FixedStr::from_bytes(b"/bin/telnetd");
            svc.service_type = ServiceType::Daemon;
            svc.restart_policy = RestartPolicy::Always;
            svc.max_restart = 10;
            svc.active = true;
            self.services[self.count] = svc;
            self.count += 1;
        }
        // sshd
        if self.count < MAX_SERVICES {
            let mut svc = ServiceDef::empty();
            svc.name = FixedStr::from_bytes(b"sshd");
            svc.command = FixedStr::from_bytes(b"/bin/sshd");
            svc.service_type = ServiceType::Daemon;
            svc.restart_policy = RestartPolicy::Always;
            svc.max_restart = 10;
            svc.active = true;
            self.services[self.count] = svc;
            self.count += 1;
        }
    }

    /// Boot all services in dependency order.
    fn boot_services(&mut self) {
        let (graph, unresolvable) = DepGraph::build(&self.services, self.count);

        // Mark services with unresolvable deps as PermanentlyStopped.
        let mut i = 0;
        while i < self.count {
            if unresolvable[i] && self.services[i].active {
                write_str(STDOUT_FILENO, "init: service '");
                write(STDOUT_FILENO, self.services[i].name.as_bytes());
                write_str(
                    STDOUT_FILENO,
                    "' permanently stopped (unresolvable dependency)\n",
                );
                self.services[i].status = ServiceStatus::PermanentlyStopped;
            }
            i += 1;
        }

        // Check for cycles.
        if graph.has_cycle(self.count) {
            write_str(
                STDOUT_FILENO,
                "init: WARNING: dependency cycle detected among services, starting in file order\n",
            );
            // Log which services are involved.
            write_str(STDOUT_FILENO, "init: cycle may involve: ");
            let mut first = true;
            let mut ci = 0;
            while ci < self.count {
                if self.services[ci].active
                    && self.services[ci].status != ServiceStatus::PermanentlyStopped
                    && graph.depends_count[ci] > 0
                {
                    if !first {
                        write_str(STDOUT_FILENO, ", ");
                    }
                    write(STDOUT_FILENO, self.services[ci].name.as_bytes());
                    first = false;
                }
                ci += 1;
            }
            write_str(STDOUT_FILENO, "\n");
            // Fall through to start in file order.
            let mut si = 0;
            while si < self.count {
                if self.services[si].active
                    && self.services[si].status != ServiceStatus::PermanentlyStopped
                {
                    self.start_service(si);
                }
                si += 1;
            }
            return;
        }

        let (order, order_len) = graph.topo_order(&self.services, self.count);

        let mut i = 0;
        while i < order_len {
            let idx = order[i];
            if self.services[idx].active && self.services[idx].status == ServiceStatus::NeverStarted
            {
                // Check all deps are Running (or Stopped for oneshot deps).
                let deps_ready = self.check_deps_ready(&graph, idx);
                if deps_ready {
                    self.start_service(idx);
                    // For daemons, give a brief moment to start.
                    if self.services[idx].service_type == ServiceType::Daemon {
                        // Small yield to let the child exec.
                        nanosleep(0);
                    }
                } else {
                    write_str(STDOUT_FILENO, "init: skipping '");
                    write(STDOUT_FILENO, self.services[idx].name.as_bytes());
                    write_str(STDOUT_FILENO, "' (deps not ready)\n");
                    self.services[idx].status = ServiceStatus::PermanentlyStopped;
                    self.services[idx].last_change_time = now_epoch_secs();
                }
            }
            i += 1;
        }
    }

    fn check_deps_ready(&self, graph: &DepGraph, idx: usize) -> bool {
        let mut d = 0;
        while d < graph.depends_count[idx] {
            let dep_idx = graph.depends[idx][d];
            match self.services[dep_idx].status {
                ServiceStatus::Running => {}
                ServiceStatus::Stopped(0)
                    if self.services[dep_idx].service_type == ServiceType::Oneshot => {}
                _ => return false,
            }
            d += 1;
        }
        true
    }

    /// Fork+exec a single service.
    fn start_service(&mut self, idx: usize) {
        let svc = &mut self.services[idx];

        // Validate state transition (diagnostic only — log but don't block).
        if !svc.status.try_transition(ServiceStatus::Starting) {
            write_str(
                STDOUT_FILENO,
                "init: warning: unexpected state transition to Starting for '",
            );
            write(STDOUT_FILENO, svc.name.as_bytes());
            write_str(STDOUT_FILENO, "'\n");
        }

        svc.status = ServiceStatus::Starting;

        write_str(STDOUT_FILENO, "init: starting '");
        write(STDOUT_FILENO, svc.name.as_bytes());
        write_str(STDOUT_FILENO, "'\n");

        let pid = fork();
        if pid < 0 {
            write_str(STDOUT_FILENO, "init: fork failed for '");
            write(STDOUT_FILENO, svc.name.as_bytes());
            write_str(STDOUT_FILENO, "'\n");
            svc.status = ServiceStatus::Stopped(-1);
            svc.last_change_time = now_epoch_secs();
            // Note: cannot call self.write_status_file() here because svc borrows self.
            // Status file will be written on the next transition or periodic write.
            return;
        }

        if pid == 0 {
            // Child: build path with null terminator and exec.
            let mut path_buf = [0u8; MAX_CMD + 1];
            let path_len = svc.command.write_null_terminated(&mut path_buf);
            let path = &path_buf[..path_len];

            let envp: [*const u8; 5] = [
                ENV_PATH.as_ptr(),
                ENV_HOME.as_ptr(),
                ENV_TERM.as_ptr(),
                ENV_EDITOR.as_ptr(),
                core::ptr::null(),
            ];

            // Build argv: argv[0] = command path.
            let argv: [*const u8; 2] = [path.as_ptr(), core::ptr::null()];
            let ret = execve(path, &argv, &envp);

            write_str(STDOUT_FILENO, "init: execve failed for '");
            write(STDOUT_FILENO, svc.name.as_bytes());
            write_str(STDOUT_FILENO, "' (");
            write_u64(STDOUT_FILENO, (-ret) as u64);
            write_str(STDOUT_FILENO, ")\n");
            exit(127);
        }

        // Parent.
        let now = now_epoch_secs();
        self.services[idx].status = ServiceStatus::Running;
        self.services[idx].pid = pid as i32;
        self.services[idx].last_start_time = now;
        self.services[idx].last_change_time = now;
        self.pid_table.insert(pid as i32, idx);

        write_str(STDOUT_FILENO, "init: started '");
        write(STDOUT_FILENO, self.services[idx].name.as_bytes());
        write_str(STDOUT_FILENO, "' pid=");
        write_u64(STDOUT_FILENO, pid as u64);
        write_str(STDOUT_FILENO, "\n");

        // Write status immediately on state transition.
        self.write_status_file();
    }

    /// Handle a reaped child PID with its exit status.
    ///
    /// Wait status encoding:
    /// - If bit 7 set: process was killed by signal, bits 0-6 = signal number
    /// - Otherwise: bits 8-15 = exit code
    fn handle_child_exit(&mut self, pid: i32, status: i32) {
        match self.pid_table.lookup(pid) {
            Some(idx) => {
                self.pid_table.remove(pid);

                let signaled = (status & 0x80) != 0;
                let (exit_code, signal_num) = if signaled {
                    (status & 0x7f, status & 0x7f) // signal number in lower 7 bits
                } else {
                    (((status >> 8) & 0xff), 0)
                };

                self.services[idx].status = ServiceStatus::Stopped(exit_code);
                self.services[idx].pid = 0;
                self.services[idx].last_change_time = now_epoch_secs();

                write_str(STDOUT_FILENO, "init: service '");
                write(STDOUT_FILENO, self.services[idx].name.as_bytes());
                if signaled {
                    write_str(STDOUT_FILENO, "' killed by signal ");
                    write_u64(STDOUT_FILENO, signal_num as u64);
                } else if exit_code == 0 {
                    write_str(STDOUT_FILENO, "' exited normally");
                } else {
                    write_str(STDOUT_FILENO, "' exited with error ");
                    write_u64(STDOUT_FILENO, exit_code as u64);
                }
                write_str(STDOUT_FILENO, "\n");

                // Write status immediately on state transition.
                self.write_status_file();

                // Check restart policy if not shutting down.
                if !self.shutdown_requested {
                    self.maybe_restart(idx, exit_code, signaled);
                }
            }
            None => {
                // Not a managed service — could be a login shell or other child.
                if pid == self.login_pid {
                    write_str(
                        STDOUT_FILENO,
                        "\ninit: session ended, respawning login...\n",
                    );
                    self.login_pid = spawn_login();
                }
            }
        }
    }

    /// Restart a service if its restart policy allows it.
    ///
    /// `signaled` indicates the process was killed by a signal (not a normal exit).
    fn maybe_restart(&mut self, idx: usize, exit_code: i32, signaled: bool) {
        let svc = &self.services[idx];

        let should_restart = match svc.restart_policy {
            RestartPolicy::Always => true,
            // OnFailure restarts on error exit OR signal death, but NOT clean exit.
            RestartPolicy::OnFailure => signaled || exit_code != 0,
            RestartPolicy::Never => false,
        };

        if !should_restart {
            return;
        }

        if svc.restart_count >= svc.max_restart {
            write_str(STDOUT_FILENO, "init: service '");
            write(STDOUT_FILENO, svc.name.as_bytes());
            write_str(
                STDOUT_FILENO,
                "' exceeded max restarts, permanently stopped\n",
            );
            self.services[idx].status = ServiceStatus::PermanentlyStopped;
            self.services[idx].last_change_time = now_epoch_secs();
            self.write_status_file();
            return;
        }

        // Reset restart count if the service ran for at least 10 seconds.
        let now = now_epoch_secs();
        let last_start = self.services[idx].last_start_time;
        if last_start > 0 && now >= last_start + 10 {
            self.services[idx].restart_count = 0;
        }

        let delay = restart_delay_secs(self.services[idx].restart_count);

        write_str(STDOUT_FILENO, "init: restarting '");
        write(STDOUT_FILENO, self.services[idx].name.as_bytes());
        write_str(STDOUT_FILENO, "' after ");
        write_u64(STDOUT_FILENO, delay);
        write_str(STDOUT_FILENO, "s delay (attempt ");
        write_u64(STDOUT_FILENO, (self.services[idx].restart_count + 1) as u64);
        write_str(STDOUT_FILENO, "/");
        write_u64(STDOUT_FILENO, self.services[idx].max_restart as u64);
        write_str(STDOUT_FILENO, ")\n");

        self.services[idx].restart_count += 1;

        // Progressive backoff delay.
        let mut d: u64 = 0;
        while d < delay {
            nanosleep(1);
            d += 1;
        }

        self.start_service(idx);
    }

    /// Orderly shutdown of all services in reverse dependency order.
    fn shutdown_services(&mut self) {
        write_str(STDOUT_FILENO, "init: shutting down services...\n");
        let shutdown_start = now_epoch_secs();

        // Iteratively find a running service whose dependents are all stopped,
        // send SIGTERM, wait, then SIGKILL if needed.
        let (graph, _unresolvable) = DepGraph::build(&self.services, self.count);

        loop {
            let mut found = false;
            let mut any_running = false;

            let mut i = 0;
            while i < self.count {
                if !self.services[i].active {
                    i += 1;
                    continue;
                }
                match self.services[i].status {
                    ServiceStatus::Running | ServiceStatus::Starting => {
                        any_running = true;
                        // Check if all dependents are stopped.
                        if self.all_dependents_stopped(&graph, i) {
                            let svc_start = now_epoch_secs();
                            write_str(STDOUT_FILENO, "init: shutdown: stopping '");
                            write(STDOUT_FILENO, self.services[i].name.as_bytes());
                            write_str(STDOUT_FILENO, "'...\n");

                            let clean = self.stop_service(i);
                            let elapsed = now_epoch_secs().saturating_sub(svc_start);

                            write_str(STDOUT_FILENO, "init: shutdown: '");
                            write(STDOUT_FILENO, self.services[i].name.as_bytes());
                            if clean {
                                write_str(STDOUT_FILENO, "' stopped cleanly (");
                            } else {
                                write_str(STDOUT_FILENO, "' force-killed (");
                            }
                            write_u64(STDOUT_FILENO, elapsed);
                            write_str(STDOUT_FILENO, "s)\n");

                            found = true;
                            break; // Re-scan after stopping one.
                        }
                    }
                    _ => {}
                }
                i += 1;
            }

            if !any_running {
                break;
            }
            if !found {
                // Deadlock — force kill all remaining.
                write_str(STDOUT_FILENO, "init: force-killing remaining services\n");
                let mut i = 0;
                while i < self.count {
                    if self.services[i].active && self.services[i].pid > 0 {
                        match self.services[i].status {
                            ServiceStatus::Running
                            | ServiceStatus::Starting
                            | ServiceStatus::Stopping => {
                                kill(self.services[i].pid, SIGKILL);
                                let mut st: i32 = 0;
                                waitpid(self.services[i].pid, &mut st, 0);
                                self.services[i].status = ServiceStatus::Stopped(-1);
                                self.services[i].pid = 0;
                            }
                            _ => {}
                        }
                    }
                    i += 1;
                }
                break;
            }
        }

        // Reap orphaned children (forked workers reparented to PID 1).
        let mut orphan_count: u32 = 0;
        let mut reap_waited: u32 = 0;
        loop {
            let mut st: i32 = 0;
            let ret = waitpid(-1, &mut st, WNOHANG);
            if ret > 0 {
                orphan_count += 1;
            } else if ret <= 0 {
                if reap_waited >= 15 {
                    break; // global timeout
                }
                if ret < 0 {
                    break; // no children left
                }
                nanosleep(1);
                reap_waited += 1;
            }
        }
        if orphan_count > 0 {
            write_str(STDOUT_FILENO, "init: reaped ");
            write_u64(STDOUT_FILENO, orphan_count as u64);
            write_str(STDOUT_FILENO, " orphaned children\n");
        }

        let total_elapsed = now_epoch_secs().saturating_sub(shutdown_start);
        write_str(STDOUT_FILENO, "init: shutdown complete (");
        write_u64(STDOUT_FILENO, total_elapsed);
        write_str(STDOUT_FILENO, "s total)\n");
        self.write_status_file();
    }

    fn all_dependents_stopped(&self, graph: &DepGraph, idx: usize) -> bool {
        let mut d = 0;
        while d < graph.required_by_count[idx] {
            let dep_idx = graph.required_by[idx][d];
            match self.services[dep_idx].status {
                ServiceStatus::Running | ServiceStatus::Starting | ServiceStatus::Stopping => {
                    return false;
                }
                _ => {}
            }
            d += 1;
        }
        true
    }

    /// Stop a single service: SIGTERM, wait per-service timeout, SIGKILL.
    /// Returns true if the service stopped cleanly, false if force-killed.
    fn stop_service(&mut self, idx: usize) -> bool {
        let pid = self.services[idx].pid;
        if pid <= 0 {
            self.services[idx].status = ServiceStatus::Stopped(0);
            self.services[idx].last_change_time = now_epoch_secs();
            self.write_status_file();
            return true;
        }

        // Validate state transition (diagnostic only — log but don't block).
        if !self.services[idx]
            .status
            .try_transition(ServiceStatus::Stopping)
        {
            write_str(
                STDOUT_FILENO,
                "init: warning: unexpected state transition to Stopping for '",
            );
            write(STDOUT_FILENO, self.services[idx].name.as_bytes());
            write_str(STDOUT_FILENO, "'\n");
        }

        self.services[idx].status = ServiceStatus::Stopping;
        write_str(STDOUT_FILENO, "init: stopping '");
        write(STDOUT_FILENO, self.services[idx].name.as_bytes());
        write_str(STDOUT_FILENO, "'\n");

        kill(pid, SIGTERM);

        // Wait up to per-service timeout for graceful exit.
        let timeout = self.services[idx].stop_timeout;
        let mut waited: u32 = 0;
        while waited < timeout {
            let mut st: i32 = 0;
            let ret = waitpid(pid, &mut st, WNOHANG);
            if ret > 0 {
                self.pid_table.remove(pid);
                let exit_code = (st >> 8) & 0xff;
                self.services[idx].status = ServiceStatus::Stopped(exit_code);
                self.services[idx].pid = 0;
                self.services[idx].last_change_time = now_epoch_secs();
                self.write_status_file();
                return true;
            }
            nanosleep(1);
            waited += 1;
        }

        // Force kill.
        write_str(STDOUT_FILENO, "init: force-killing '");
        write(STDOUT_FILENO, self.services[idx].name.as_bytes());
        write_str(STDOUT_FILENO, "'\n");
        kill(pid, SIGKILL);

        let mut st: i32 = 0;
        waitpid(pid, &mut st, 0);
        self.pid_table.remove(pid);
        self.services[idx].status = ServiceStatus::Stopped(-1);
        self.services[idx].pid = 0;
        self.services[idx].last_change_time = now_epoch_secs();
        self.write_status_file();
        false
    }

    /// Write service status to `/var/run/services.status`.
    fn write_status_file(&self) {
        let fd = open(STATUS_FILE, O_WRONLY | O_CREAT | O_TRUNC, 0o644);
        if fd < 0 {
            return;
        }

        let mut i = 0;
        while i < self.count {
            if !self.services[i].active {
                i += 1;
                continue;
            }
            let svc = &self.services[i];

            // name=<name> status=<status> pid=<pid> restarts=<count>\n
            write(fd as i32, svc.name.as_bytes());
            write_str(fd as i32, " ");

            match svc.status {
                ServiceStatus::NeverStarted => write_str(fd as i32, "never-started"),
                ServiceStatus::Starting => write_str(fd as i32, "starting"),
                ServiceStatus::Running => write_str(fd as i32, "running"),
                ServiceStatus::Stopping => write_str(fd as i32, "stopping"),
                ServiceStatus::Stopped(code) => {
                    write_str(fd as i32, "stopped:");
                    write_u64(fd as i32, code as u64);
                    0 // match arm type consistency
                }
                ServiceStatus::PermanentlyStopped => write_str(fd as i32, "permanently-stopped"),
            };

            write_str(fd as i32, " pid=");
            write_u64(fd as i32, svc.pid as u64);
            write_str(fd as i32, " restarts=");
            write_u64(fd as i32, svc.restart_count as u64);
            write_str(fd as i32, " changed=");
            write_u64(fd as i32, svc.last_change_time);
            write_str(fd as i32, "\n");

            i += 1;
        }

        close(fd as i32);
    }

    /// Check for control commands in `/var/run/init.cmd`.
    fn check_control_commands(&mut self) {
        let fd = open(CMD_FILE, O_RDONLY, 0);
        if fd < 0 {
            return;
        }

        let mut buf = [0u8; 128];
        let n = read(fd as i32, &mut buf);
        close(fd as i32);

        // Delete the command file after reading by truncating it.
        let fd2 = open(CMD_FILE, O_WRONLY | O_TRUNC, 0);
        if fd2 >= 0 {
            close(fd2 as i32);
        }

        if n <= 0 {
            return;
        }
        let n = n as usize;

        // Parse command: "start <name>", "stop <name>", "restart <name>"
        if n >= 6 && bytes_eq(&buf[..5], b"start") && buf[5] == b' ' {
            let name = trim_newline(&buf[6..n]);
            if let Some(idx) = self.find_service(name) {
                write_str(STDOUT_FILENO, "init: control: starting '");
                write(STDOUT_FILENO, name);
                write_str(STDOUT_FILENO, "'\n");
                self.services[idx].restart_count = 0;
                self.start_service(idx);
            }
        } else if n >= 5 && bytes_eq(&buf[..4], b"stop") && buf[4] == b' ' {
            let name = trim_newline(&buf[5..n]);
            if let Some(idx) = self.find_service(name) {
                write_str(STDOUT_FILENO, "init: control: stopping '");
                write(STDOUT_FILENO, name);
                write_str(STDOUT_FILENO, "'\n");
                // Set restart policy to never so reap loop won't restart it.
                self.services[idx].restart_policy = RestartPolicy::Never;
                let _ = self.stop_service(idx);
            }
        } else if n >= 8 && bytes_eq(&buf[..7], b"restart") && buf[7] == b' ' {
            let name = trim_newline(&buf[8..n]);
            if let Some(idx) = self.find_service(name) {
                write_str(STDOUT_FILENO, "init: control: restarting '");
                write(STDOUT_FILENO, name);
                write_str(STDOUT_FILENO, "'\n");
                let _ = self.stop_service(idx);
                self.services[idx].restart_count = 0;
                nanosleep(1);
                self.start_service(idx);
            }
        }
    }

    fn find_service(&self, name: &[u8]) -> Option<usize> {
        let mut i = 0;
        while i < self.count {
            if self.services[i].active && self.services[i].name.eq_bytes(name) {
                return Some(i);
            }
            i += 1;
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Write a u32 value as decimal ASCII into a buffer. Returns bytes written.
#[allow(dead_code)]
fn write_u32_to_buf(buf: &mut [u8], val: u32) -> usize {
    if val == 0 {
        if !buf.is_empty() {
            buf[0] = b'0';
        }
        return 1;
    }
    let mut tmp = [0u8; 10];
    let mut n = val;
    let mut i = 0;
    while n > 0 {
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    let len = i.min(buf.len());
    let mut j = 0;
    while j < len {
        buf[j] = tmp[i - 1 - j];
        j += 1;
    }
    len
}

/// Get current wall-clock time as epoch seconds, or 0 on error.
fn now_epoch_secs() -> u64 {
    let (sec, _nsec) = clock_gettime(0); // CLOCK_REALTIME = 0
    if sec < 0 { 0 } else { sec as u64 }
}

/// Compute restart delay in seconds based on restart count (progressive backoff).
/// 1s for first restart, 2s for second, then 5s cap.
fn restart_delay_secs(restart_count: u32) -> u64 {
    match restart_count {
        0 => 1,
        1 => 2,
        _ => 5,
    }
}

fn trim_newline(buf: &[u8]) -> &[u8] {
    let mut end = buf.len();
    while end > 0 && (buf[end - 1] == b'\n' || buf[end - 1] == b'\r' || buf[end - 1] == b' ') {
        end -= 1;
    }
    &buf[..end]
}

fn spawn_login() -> i32 {
    let pid = fork();
    if pid == 0 {
        let envp: [*const u8; 5] = [
            ENV_PATH.as_ptr(),
            ENV_HOME.as_ptr(),
            ENV_TERM.as_ptr(),
            ENV_EDITOR.as_ptr(),
            core::ptr::null(),
        ];

        let argv: [*const u8; 2] = [LOGIN_ARGV0.as_ptr(), core::ptr::null()];
        let ret = execve(LOGIN_PATH, &argv, &envp);

        write_str(STDOUT_FILENO, "init: login execve failed (");
        write_u64(STDOUT_FILENO, (-ret) as u64);
        write_str(STDOUT_FILENO, ")\n");
        exit(1);
    }
    if pid < 0 {
        write_str(STDOUT_FILENO, "init: failed to fork login\n");
        return -1;
    }
    pid as i32
}

// ---------------------------------------------------------------------------
// SIGTERM handler — sets a flag checked in the main loop.
//
// We use a static mut flag; safe because PID 1 is single-threaded and
// signal delivery is serialized by the kernel.
// ---------------------------------------------------------------------------

use core::sync::atomic::{AtomicBool, Ordering};

static SIGTERM_RECEIVED: AtomicBool = AtomicBool::new(false);

extern "C" fn sigterm_handler(_sig: i32) {
    SIGTERM_RECEIVED.store(true, Ordering::Release);
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Fds 0/1/2 are pre-opened by the kernel for PID 1.
    write_str(STDOUT_FILENO, "\nm3OS init (PID 1) — service manager\n");

    // Mount ext2 root filesystem at /.
    #[allow(clippy::manual_c_str_literals)]
    let ret = mount(b"/dev/blk0\0".as_ptr(), b"/\0".as_ptr(), b"ext2\0".as_ptr());
    if ret == 0 {
        write_str(STDOUT_FILENO, "init: / mounted (ext2)\n");
    } else {
        write_str(STDOUT_FILENO, "init: / mount failed (");
        write_u64(STDOUT_FILENO, (-ret) as u64);
        write_str(STDOUT_FILENO, ")\n");
    }

    // Make /tmp world-writable.
    syscall_lib::chmod(b"/tmp\0", 0o1777);

    // Install SIGTERM handler for orderly shutdown.
    let act = SigAction {
        sa_handler: sigterm_handler as *const () as u64,
        sa_flags: 0,
        sa_restorer: 0,
        sa_mask: 0,
    };
    rt_sigaction(SIGTERM as usize, &act, core::ptr::null_mut());

    // Initialize service manager.
    let mut mgr = ServiceManager::new();

    // Load service definitions.
    mgr.load_services();

    // Boot all services in dependency order.
    mgr.boot_services();

    // Open syslog socket now that syslogd should be running.
    mgr.open_syslog();

    // Spawn initial login session (not a managed service).
    mgr.login_pid = spawn_login();
    if mgr.login_pid < 0 {
        write_str(STDOUT_FILENO, "init: failed to spawn login\n");
        // Not fatal — services may still be running.
    }

    // Write initial status file.
    mgr.write_status_file();

    // Track iterations for periodic status writes.
    let mut loop_count: u32 = 0;

    // Main reap loop.
    loop {
        // Check for SIGTERM (shutdown request).
        let sigterm = SIGTERM_RECEIVED.load(Ordering::Acquire);
        if sigterm {
            write_str(
                STDOUT_FILENO,
                "init: SIGTERM received, initiating shutdown\n",
            );
            mgr.shutdown_requested = true;
            mgr.shutdown_services();
            write_str(STDOUT_FILENO, "init: shutdown complete\n");
            // In a real OS we would call reboot() here. Since there is no
            // reboot syscall yet, just halt in a sleep loop.
            loop {
                nanosleep(3600);
            }
        }

        // Reap children.
        let mut status: i32 = 0;
        let ret = waitpid(-1, &mut status, WNOHANG);
        if ret > 0 {
            mgr.handle_child_exit(ret as i32, status);
        }

        // Check for control commands.
        mgr.check_control_commands();

        // Periodically write status file (every ~10 iterations).
        loop_count = loop_count.wrapping_add(1);
        if loop_count.is_multiple_of(10) {
            mgr.write_status_file();
        }

        // Sleep briefly if no child was reaped to avoid busy-spinning.
        if ret <= 0 {
            nanosleep(1);
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "init: PANIC\n");
    exit(101)
}
