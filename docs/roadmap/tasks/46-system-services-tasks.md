# Phase 46 — System Services: Task List

**Status:** Complete
**Source Ref:** phase-46
**Depends on:** Phase 19 (Signals) ✅, Phase 24 (Persistent Storage) ✅, Phase 27 (User Accounts) ✅, Phase 29 (PTY) ✅, Phase 30 (Telnet) ✅, Phase 34 (Real-Time Clock) ✅, Phase 39 (Unix Domain Sockets) ✅, Phase 43 (SSH) ✅
**Goal:** A unified service manager (enhanced init), system logging daemon, cron
scheduler, and system administration commands that turn m3OS into a real
administrable server with ordered boot, automatic restart, centralized logs,
scheduled tasks, and clean shutdown.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Service definition format and init parsing | — | ✅ Done |
| B | Service lifecycle, PID table, and state machine | A | ✅ Done |
| C | `service` command | A, B | ✅ Done |
| D | System logging (`syslogd` + `logger`) | — | ✅ Done |
| E | Scheduled tasks (`crond` + `crontab`) | — | ✅ Done |
| F | Kernel shutdown/reboot support | — | ✅ Done |
| G | System administration commands | D, F | ✅ Done |
| H | Integration testing and documentation | A–G | ✅ Done |

---

## Track A — Service Definition Format and Init Parsing

Define the service file format and teach init to read it.

### A.1 — Design the service definition file format

**File:** `docs/roadmap/46-system-services.md`
**Symbol:** `/etc/services.d/*.conf` (format specification)
**Why it matters:** The service definition file is the single source of truth
for each managed service: its name, binary path, type, restart policy, and
dependencies. Using shell-variable syntax keeps parsing simple in a `no_std`
userspace binary. Getting this right first avoids rework across all other tracks.

**Acceptance:**
- [ ] Format documented with required fields: `name`, `command`, `type`, `restart`, `depends`
- [ ] Format uses `key=value` line syntax (one field per line)
- [ ] `type` supports at least `daemon` (long-running) and `oneshot` (run-once)
- [ ] `restart` supports `always`, `on-failure`, and `never`
- [ ] `max_restart` field (default 10) caps restart attempts before permanent stop
- [ ] `depends` is a comma-separated list of service names (or empty)
- [ ] At least three example service files exist: `sshd.conf`, `telnetd.conf`, `syslogd.conf`

### A.2 — Create service definition files for existing daemons

**Files:**
- `kernel/initrd/etc/services.d/sshd.conf`
- `kernel/initrd/etc/services.d/telnetd.conf`
- `kernel/initrd/etc/services.d/syslogd.conf`
- `kernel/initrd/etc/services.d/crond.conf`

**Symbol:** `/etc/services.d/` (service definitions)
**Why it matters:** These files replace the hardcoded `spawn_telnetd()` and
`spawn_sshd()` calls in init. Creating them alongside the format design
validates the format immediately and provides test data for the parser.

**Acceptance:**
- [ ] `sshd.conf` declares `name=sshd`, `command=/sbin/sshd`, `type=daemon`, `restart=always`, `max_restart=10`, `depends=syslogd`
- [ ] `telnetd.conf` declares `name=telnetd`, `command=/sbin/telnetd`, `type=daemon`, `restart=always`, `max_restart=10`, `depends=syslogd`
- [ ] `syslogd.conf` declares `name=syslogd`, `command=/sbin/syslogd`, `type=daemon`, `restart=always`, `max_restart=10`, `depends=`
- [ ] `crond.conf` declares `name=crond`, `command=/sbin/crond`, `type=daemon`, `restart=always`, `max_restart=10`, `depends=syslogd`
- [ ] Files are included in the ext2 image via xtask

### A.3 — Implement service definition parser in init

**File:** `userspace/init/src/main.rs`
**Symbol:** `parse_service_def`, `ServiceDef`
**Why it matters:** Init must read all `.conf` files from `/etc/services.d/` and
parse them into structured `ServiceDef` values. This replaces the hardcoded
service list and is the foundation for dependency ordering and lifecycle
management.

**Acceptance:**
- [ ] `ServiceDef` struct holds name, command, type, restart policy, max_restart cap, and dependency list
- [ ] `parse_service_def(path)` reads a `.conf` file and returns a `ServiceDef`
- [ ] Init scans `/etc/services.d/` at startup and parses all `.conf` files
- [ ] Malformed files produce a warning to serial and are skipped (not a fatal error)
- [ ] `max_restart` defaults to 10 if omitted from the file

### A.4 — Build dependency graph with bidirectional edges

**File:** `userspace/init/src/main.rs`
**Symbol:** `build_dep_graph`, `DepGraph`
**Why it matters:** Services must start in dependency order — syslogd before
sshd, for example. Following rustysd's approach (MIT), the parser fills both
`depends` (forward) and `required_by` (reverse) edges at parse time. This
makes startup ordering a simple forward walk and shutdown a reverse walk
without needing separate sorted lists. Cycles must be detected and reported.

**Acceptance:**
- [ ] `DepGraph` stores both `depends` and `required_by` edges for each service
- [ ] Forward edges are parsed from `.conf` files; reverse edges are derived automatically
- [ ] Startup order: a service starts only after all its `depends` are in `Running` state
- [ ] Circular dependencies are detected via DFS cycle check and produce a clear error
- [ ] Missing dependencies (referenced but no `.conf` file) produce a warning

---

## Track B — Service Lifecycle, PID Table, and State Machine

Implement the core data structures and lifecycle logic for service management.

### B.1 — Implement ServiceStatus state machine and PID table

**File:** `userspace/init/src/main.rs`
**Symbol:** `ServiceStatus`, `PidTable`, `PidEntry`
**Why it matters:** Before starting services, init needs the data structures
that track their lifecycle. The `ServiceStatus` enum (inspired by rustysd, MIT)
models the full lifecycle: `NeverStarted -> Starting -> Running -> Stopping ->
Stopped(exit_code)`. The `PidTable` maps child PIDs to `PidEntry` values that
identify which service (or non-service child) owns each PID, enabling O(1)
lookup on SIGCHLD instead of scanning all services.

**Acceptance:**
- [ ] `ServiceStatus` enum with variants: `NeverStarted`, `Starting`, `Running`, `Stopping`, `Stopped(i32)`
- [ ] `PidTable` (e.g., `BTreeMap<u32, PidEntry>`) maps PIDs to service names
- [ ] `PidEntry` distinguishes managed services from non-managed children (login shells)
- [ ] State transitions are enforced (e.g., cannot go from `Stopped` to `Stopping`)

### B.2 — Replace hardcoded spawning with service-driven boot

**File:** `userspace/init/src/main.rs`
**Symbol:** `boot_services`
**Why it matters:** Currently init calls `spawn_telnetd()` and `spawn_sshd()`
directly. This task replaces those calls with a dependency-ordered walk of the
service graph: for each service, check that all `depends` are `Running`, then
fork+exec. This is the key architectural change from hardcoded init to
data-driven service manager.

**Acceptance:**
- [ ] Init starts services by walking the dependency graph in order
- [ ] Each service is spawned via `fork()` + `execve(command)`
- [ ] Spawned PID is inserted into the `PidTable` and service status set to `Starting` then `Running`
- [ ] Old hardcoded `spawn_telnetd()` and `spawn_sshd()` are removed
- [ ] Services start in correct dependency order (syslogd before sshd/telnetd)

### B.3 — Implement SIGCHLD-based service exit detection via PID table

**File:** `userspace/init/src/main.rs`
**Symbol:** `handle_sigchld`
**Why it matters:** When a managed service exits (crash or normal), init must
detect it via SIGCHLD and update the service state. Using the PID table
(inspired by rustysd, MIT), each reaped PID is looked up in O(1) to find
the owning service, avoiding a linear scan. The service's `ServiceStatus`
transitions to `Stopped(exit_code)`.

**Acceptance:**
- [ ] Init installs a SIGCHLD handler via `rt_sigaction`
- [ ] On SIGCHLD, init calls `waitpid(-1, WNOHANG)` in a loop to reap all exited children
- [ ] Each reaped PID is looked up in the `PidTable` to identify the owning service
- [ ] Service status transitions to `Stopped(exit_code)` and PID is removed from the table
- [ ] Non-managed children (login shells) are reaped and their `PidEntry` removed without error

### B.4 — Implement automatic service restart with max_restart cap

**File:** `userspace/init/src/main.rs`
**Symbol:** `maybe_restart_service`
**Why it matters:** The `restart=always` policy means init must respawn the
service when it exits unexpectedly. A brief backoff delay prevents a crashing
service from consuming all resources. The `max_restart` cap (inspired by
rustysd's `max_deaths`, MIT) ensures a permanently broken service is not
restarted indefinitely. This is the core supervision feature that makes the
system self-healing without being self-destructive.

**Acceptance:**
- [ ] Services with `restart=always` are restarted after any exit
- [ ] Services with `restart=on-failure` are restarted only on non-zero exit status
- [ ] Services with `restart=never` are not restarted
- [ ] Restart count is tracked per-service; exceeding `max_restart` marks the service permanently stopped
- [ ] A minimum delay (e.g., 1 second) is enforced between restart attempts
- [ ] Restart count and max_restart are visible in `service status` output

### B.5 — Implement iterative reverse-dependency shutdown

**File:** `userspace/init/src/main.rs`
**Symbol:** `shutdown_services`, `next_stoppable`
**Why it matters:** On shutdown or reboot, init must stop services in reverse
dependency order (sshd before syslogd). Following rustysd's approach (MIT),
shutdown iteratively finds a running service whose `required_by` dependents
are all already stopped, stops it (SIGTERM then SIGKILL), and repeats until
no running services remain. This avoids maintaining a pre-computed reverse
sorted list.

**Acceptance:**
- [ ] `next_stoppable()` returns a running service whose dependents are all stopped
- [ ] `shutdown_services()` loops calling `next_stoppable()` until no running services remain
- [ ] Each service receives SIGTERM first; after a timeout (e.g., 5 seconds), SIGKILL
- [ ] Init waits for each service process to exit before moving to the next
- [ ] After all services stop, init syncs filesystems and calls `sys_reboot()`
- [ ] syslogd is stopped last (since sshd/telnetd/crond all depend on it)

---

## Track C — `service` Command

User-facing command to interact with the service manager.

### C.1 — Create the `service` command binary

**File:** `userspace/coreutils-rs/src/service.rs`
**Symbol:** `main` (service command entry point)
**Why it matters:** The `service` command is the primary administration interface
for the service manager. It must communicate with init to request operations
and query status. Using a signal-based or file-based protocol keeps the IPC
simple without needing a dedicated control socket.

**Acceptance:**
- [ ] `service` binary exists in coreutils-rs and is installed at `/usr/bin/service`
- [ ] `service` with no arguments prints usage: `service {start|stop|restart|status|list} [name]`
- [ ] Unknown subcommands produce an error message

### C.2 — Implement `service list`

**File:** `userspace/coreutils-rs/src/service.rs`
**Symbol:** `cmd_list`
**Why it matters:** `service list` reads service definition files and the
service state file to display all services with their current status. This is
the most-used subcommand for system administrators checking what is running.

**Acceptance:**
- [ ] Lists all services from `/etc/services.d/` with name, status (running/stopped), and PID
- [ ] Running services show their PID and uptime
- [ ] Stopped services show their last exit status
- [ ] Output is formatted in a readable table

### C.3 — Implement `service status <name>`

**File:** `userspace/coreutils-rs/src/service.rs`
**Symbol:** `cmd_status`
**Why it matters:** Shows detailed status for a single service including PID,
uptime, restart count, and dependencies. This is essential for debugging
service issues.

**Acceptance:**
- [ ] Shows service name, status (running/stopped), PID, and uptime
- [ ] Shows restart count and last exit status
- [ ] Shows the service's dependencies
- [ ] Reports an error if the service name is not found

### C.4 — Implement `service start/stop/restart <name>`

**File:** `userspace/coreutils-rs/src/service.rs`
**Symbol:** `cmd_start`, `cmd_stop`, `cmd_restart`
**Why it matters:** These commands allow administrators to manually control
services. `stop` sends SIGTERM (then SIGKILL) to the service process. `start`
tells init to spawn it. `restart` is stop-then-start. The communication
mechanism with init (signal + state file, or control pipe) must be defined here.

**Acceptance:**
- [ ] `service stop <name>` sends SIGTERM to the service's PID and marks it as manually stopped
- [ ] `service start <name>` tells init to start a stopped service
- [ ] `service restart <name>` performs stop then start
- [ ] Manually stopped services are not auto-restarted by init
- [ ] Reports success/failure to the user

---

## Track D — System Logging (`syslogd` + `logger`)

Centralized logging daemon and command-line log tool.

### D.1 — Create the `syslogd` daemon binary

**Files:**
- `userspace/syslogd/Cargo.toml`
- `userspace/syslogd/src/main.rs`

**Symbol:** `_start` (syslogd entry point)
**Why it matters:** syslogd is the central logging hub. All services send log
messages to it via the `/dev/log` Unix domain socket. It formats and writes
them to persistent log files, providing the audit trail essential for system
administration and debugging.

**Acceptance:**
- [ ] `syslogd` crate created under `userspace/` as a `no_std` binary
- [ ] Binds a Unix domain socket at `/dev/log` (AF_UNIX, SOCK_DGRAM or SOCK_STREAM)
- [ ] Main loop: accept connections, read messages, format, write to log file
- [ ] Log format: `YYYY-MM-DD HH:MM:SS hostname service[pid]: message`
- [ ] Writes to `/var/log/messages` (created if it doesn't exist)

### D.2 — Implement log message parsing and formatting

**File:** `userspace/syslogd/src/main.rs`
**Symbol:** `format_log_entry`, `parse_message`
**Why it matters:** Services send raw messages (or messages with a priority
prefix like `<13>message`). syslogd must parse the priority, extract the
service identity, add a timestamp from `clock_gettime`, and format the
complete log line before writing it.

**Acceptance:**
- [ ] Parses optional `<priority>` prefix from messages
- [ ] Extracts or defaults service name and PID
- [ ] Formats timestamp using `clock_gettime(CLOCK_REALTIME)` and time conversion
- [ ] Produces correctly formatted log lines matching the syslog format

### D.3 — Write kernel messages to `/var/log/kern.log`

**File:** `userspace/syslogd/src/main.rs`
**Symbol:** `drain_kernel_log`
**Why it matters:** Kernel messages (from the `log` crate / serial output) are
currently only visible on serial. Writing them to a persistent file means they
survive after boot and are available for post-mortem analysis. syslogd reads
the kernel ring buffer (via `sys_syslog` or dmesg interface) and writes to a
separate file.

**Acceptance:**
- [ ] Kernel messages are read from the kernel log buffer (dmesg or a dedicated syscall)
- [ ] Written to `/var/log/kern.log` with timestamps
- [ ] New kernel messages are periodically drained (not just at startup)
- [ ] Kernel log and userspace log are in separate files

### D.4 — Create the `logger` command

**File:** `userspace/coreutils-rs/src/logger.rs`
**Symbol:** `main` (logger command)
**Why it matters:** `logger` is the command-line interface to syslog. It connects
to `/dev/log` and sends a message, allowing shell scripts and interactive users
to write to the system log. It is also used for testing syslogd.

**Acceptance:**
- [ ] `logger "test message"` sends the message to `/dev/log`
- [ ] Message appears in `/var/log/messages` with correct timestamp and formatting
- [ ] Supports `-t tag` to set the service/tag name
- [ ] Supports `-p priority` to set the priority level (optional)

---

## Track E — Scheduled Tasks (`crond` + `crontab`)

Cron daemon and crontab management command.

### E.1 — Create the `crond` daemon binary

**Files:**
- `userspace/crond/Cargo.toml`
- `userspace/crond/src/main.rs`

**Symbol:** `_start` (crond entry point)
**Why it matters:** crond enables scheduled automation — the ability to run
commands at specific times without human intervention. This is how real servers
run backups, log rotation, and maintenance tasks. The daemon reads crontab
files, sleeps until the next job, wakes up and executes it.

**Acceptance:**
- [ ] `crond` crate created under `userspace/` as a `no_std` binary
- [ ] Reads `/etc/crontab` at startup
- [ ] Reads per-user crontabs from `/var/spool/cron/<user>`
- [ ] Main loop: compute next job time, `nanosleep` until then, fork+exec the command
- [ ] Logs job execution to syslog via `/dev/log`

### E.2 — Implement crontab format parser

**File:** `userspace/crond/src/main.rs`
**Symbol:** `parse_crontab`, `CronEntry`
**Why it matters:** The crontab format (`minute hour day month weekday command`)
is a Unix standard. Parsing it correctly — including wildcards (`*`), ranges
(`1-5`), and step values (`*/5`) — is essential for the daemon to schedule
jobs accurately.

**Acceptance:**
- [ ] `CronEntry` struct holds minute, hour, day, month, weekday fields and command string
- [ ] Parser handles numeric values, `*` (any), ranges (`1-5`), and step values (`*/5`)
- [ ] Parser handles special strings: `@reboot`, `@hourly`, `@daily`
- [ ] Comment lines (starting with `#`) and blank lines are skipped
- [ ] Malformed lines produce a warning and are skipped

### E.3 — Implement next-run-time computation

**File:** `userspace/crond/src/main.rs`
**Symbol:** `next_run_time`, `matches_time`
**Why it matters:** crond must compute when each job next fires so it can sleep
efficiently. Given the current wall-clock time (from `clock_gettime`) and a
cron schedule, it must find the next matching minute. This is the core
scheduling algorithm.

**Acceptance:**
- [ ] `matches_time(entry, time)` returns true if the cron entry matches the given time
- [ ] `next_run_time(entry, now)` returns the next Unix timestamp when the entry fires
- [ ] Correctly handles month/day boundaries and wildcard combinations
- [ ] `@reboot` entries fire once at crond startup
- [ ] `@hourly` maps to `0 * * * *`, `@daily` maps to `0 0 * * *`

### E.4 — Implement job execution and SIGHUP reload

**File:** `userspace/crond/src/main.rs`
**Symbol:** `execute_job`, `reload_crontabs`
**Why it matters:** When a job fires, crond forks a child process and execs the
command (as the crontab's owner user). SIGHUP support lets the `crontab`
command signal crond to re-read its files without a restart.

**Acceptance:**
- [ ] Jobs are executed via `fork()` + `execve()` as the owning user
- [ ] Job output (stdout/stderr) is captured and logged to syslog
- [ ] SIGHUP causes crond to re-read all crontab files
- [ ] `@reboot` jobs are executed exactly once at crond startup

### E.5 — Create the `crontab` command

**File:** `userspace/coreutils-rs/src/crontab.rs`
**Symbol:** `main` (crontab command)
**Why it matters:** `crontab` is the user interface for managing scheduled jobs.
The current minimal implementation focuses on inspecting and removing per-user
crontabs: `-l` lists the current user's crontab and `-r` removes it. Interactive
editing via `$EDITOR` is deferred.

**Acceptance:**
- [ ] `crontab -l` prints the current user's crontab from `/var/spool/cron/<user>`
- [ ] `crontab -r` removes the current user's crontab
- [ ] After removal, sends SIGHUP to crond to trigger reload
- [ ] Root can manage other users' crontabs: `crontab -u <user> -l`
- [ ] Documentation notes that interactive `crontab -e` editing remains deferred

---

## Track F — Kernel Shutdown/Reboot Support

Add kernel-side support for orderly system halt and restart.

### F.1 — Implement `sys_reboot()` syscall

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_reboot`
**Why it matters:** The kernel must provide a mechanism for userspace to request
system halt or restart. Linux uses syscall 169 with magic numbers to prevent
accidental invocation. Our implementation needs at minimum halt and restart
commands, restricted to root (UID 0).

**Acceptance:**
- [ ] Syscall 169 (`sys_reboot`) is implemented in the syscall table
- [ ] Accepts a command argument: `HALT` (power off), `RESTART` (reboot)
- [ ] Only UID 0 processes can invoke it (returns `-EPERM` for others)
- [ ] `HALT` syncs filesystems and halts the CPU (loop + hlt, or ACPI shutdown)
- [ ] `RESTART` syncs filesystems and performs a CPU triple-fault reset

### F.2 — Implement kernel shutdown sequence

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `kernel_shutdown`, `sync_filesystems`
**Why it matters:** Before halting the CPU, the kernel must flush all dirty
filesystem buffers to disk and cleanly shut down device drivers. Skipping
this risks data loss on the persistent ext2 partition.

**Acceptance:**
- [ ] `sync_filesystems()` flushes all dirty buffers to disk (ext2 sync)
- [ ] VirtIO-blk driver is quiesced (all pending I/O completed)
- [ ] A "System halted" or "Restarting..." message is printed to serial
- [ ] QEMU exit code is appropriate (success for intentional shutdown)

### F.3 — Add `sys_reboot` to syscall-lib

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbol:** `reboot`, `SYS_REBOOT`
**Why it matters:** Userspace commands (`shutdown`, `reboot`) need a wrapper
function to invoke the new syscall. This follows the same pattern as all
other syscall wrappers in syscall-lib.

**Acceptance:**
- [ ] `SYS_REBOOT` constant (169) added to syscall-lib
- [ ] `reboot(cmd: u32) -> isize` wrapper function added
- [ ] Constants for `REBOOT_HALT` and `REBOOT_RESTART` defined
- [ ] Wrapper is usable from shutdown/reboot command binaries

---

## Track G — System Administration Commands

Shell commands for system administration.

### G.1 — Create `shutdown` command

**File:** `userspace/coreutils-rs/src/shutdown.rs`
**Symbol:** `main` (shutdown command)
**Why it matters:** `shutdown` is the primary interface for orderly system halt.
It must signal init to begin the shutdown sequence (stop all services), then
invoke `sys_reboot(HALT)`. Real systems support delayed shutdown and broadcast
warnings; our minimal version is immediate.

**Acceptance:**
- [ ] `shutdown` sends SIGTERM to init (PID 1) to trigger service shutdown
- [ ] Init stops all services in reverse dependency order
- [ ] After services are stopped, calls `sys_reboot(HALT)`
- [ ] Prints "System is going down for halt..." to all terminals
- [ ] Only root can execute shutdown (exits with error for non-root)

### G.2 — Create `reboot` command

**File:** `userspace/coreutils-rs/src/reboot.rs`
**Symbol:** `main` (reboot command)
**Why it matters:** `reboot` is identical to `shutdown` but passes the restart
command instead of halt, causing the system to restart rather than power off.

**Acceptance:**
- [ ] `reboot` triggers the same service shutdown sequence as `shutdown`
- [ ] Calls `sys_reboot(RESTART)` instead of `HALT`
- [ ] Prints "System is going down for reboot..." to all terminals
- [ ] Only root can execute reboot

### G.3 — Create or update `hostname` command

**File:** `userspace/coreutils-rs/src/hostname.rs`
**Symbol:** `main` (hostname command)
**Why it matters:** `hostname` displays or sets the system's hostname, which is
used in log messages, shell prompts, and network identification. If a hostname
command already exists, it may need set support added.

**Acceptance:**
- [ ] `hostname` with no arguments prints the current hostname
- [ ] `hostname <name>` sets the hostname (root only)
- [ ] Hostname is stored in a kernel variable or `/etc/hostname` file
- [ ] syslogd uses the hostname in log line formatting

### G.4 — Create `who` and `w` commands

**File:** `userspace/coreutils-rs/src/who.rs`
**Symbol:** `main` (who command)
**Why it matters:** `who` shows currently logged-in users by reading the PTY
session table or a utmp-style file. This is essential for multi-user system
administration — knowing who is connected and from where.

**Acceptance:**
- [ ] `who` lists all logged-in users with username, terminal (PTY), and login time
- [ ] Data sourced from PTY session table, `/var/run/utmp`, or process table
- [ ] Shows remote host for telnet/SSH sessions
- [ ] `w` variant also shows idle time and current command (can be same binary with different behavior)

### G.5 — Create `last` command

**File:** `userspace/coreutils-rs/src/last.rs`
**Symbol:** `main` (last command)
**Why it matters:** `last` shows recent login history by reading a wtmp-style
log file. Login and logout events must be recorded by login/sshd/telnetd for
this to work.

**Acceptance:**
- [ ] `last` reads `/var/log/wtmp` and displays login/logout history
- [ ] Shows username, terminal, remote host, login time, and session duration
- [ ] Login events are recorded by login, sshd, and telnetd
- [ ] Logout events are recorded when sessions end

---

## Track H — Integration Testing and Documentation

Validate the complete system services stack and update documentation.

### H.1 — End-to-end test: boot → services start → cron runs → shutdown

**Files:**
- `userspace/init/src/main.rs`
- `kernel/initrd/etc/services.d/*.conf`

**Symbol:** (integration test)
**Why it matters:** The acceptance criteria require the full lifecycle to work:
boot with automatic service startup in dependency order, cron job execution,
and clean shutdown. This test validates the entire Phase 46 stack.

**Acceptance:**
- [ ] System boots and services start in correct dependency order
- [ ] `service list` shows all managed services as running
- [ ] `service status sshd` shows PID and uptime
- [ ] A cron job scheduled for every minute executes on time
- [ ] `shutdown` cleanly stops all services and halts the system

### H.2 — Test service restart on failure and max_restart cap

**Files:**
- `userspace/init/src/main.rs`

**Symbol:** `maybe_restart_service` (restart test)
**Why it matters:** The auto-restart feature is a core value proposition of
the service manager. Verifying it works correctly — that manually stopped
services are NOT restarted, and that the max_restart cap prevents infinite
crash loops — is essential.

**Acceptance:**
- [ ] Killing a `restart=always` service with `kill -9` causes init to restart it within seconds
- [ ] `service stop <name>` prevents auto-restart (manually stopped)
- [ ] `restart=on-failure` services restart on non-zero exit but not on clean exit
- [ ] `restart=never` services are never restarted
- [ ] A service that crashes more than `max_restart` times is marked permanently stopped and not restarted

### H.3 — Test syslog and cron integration

**Files:**
- `userspace/syslogd/src/main.rs`
- `userspace/crond/src/main.rs`

**Symbol:** (syslog + cron test)
**Why it matters:** Services should log to syslog, and cron jobs should produce
log entries. Verifying the integration between all three subsystems (services,
logging, scheduling) confirms the system works as a cohesive whole.

**Acceptance:**
- [ ] `logger "test message"` appears in `/var/log/messages`
- [ ] Service start/stop events appear in `/var/log/messages`
- [ ] Cron job execution is logged to syslog
- [ ] `who` shows currently logged-in users after SSH/telnet login

### H.4 — Verify no regressions in existing tests

**Files:**
- `kernel/tests/*.rs`
- `xtask/src/main.rs`

**Symbol:** (all existing tests)
**Why it matters:** Adding new daemons, syscalls, and init changes must not
break existing functionality. All pre-existing tests must continue to pass.

**Acceptance:**
- [ ] `cargo xtask check` passes (clippy + fmt)
- [ ] `cargo xtask test` passes (all existing QEMU tests)
- [ ] `cargo test -p kernel-core` passes (host-side unit tests)

### H.5 — Update documentation and roadmap

**Files:**
- `docs/roadmap/46-system-services.md`
- `docs/roadmap/README.md`
- `AGENTS.md`

**Symbol:** (documentation)
**Why it matters:** The design doc needs final implementation details, the
roadmap README needs the task list link, and AGENTS.md needs references to
the new daemons and commands.

**Acceptance:**
- [ ] Design doc status updated to `Complete` when all tasks done
- [ ] Roadmap README row updated with task list link and status
- [ ] AGENTS.md updated with syslogd, crond, service command references
- [ ] Learning doc created or updated for Phase 46 concepts (service management, syslog, cron)

---

## Documentation Notes

- Phase 46 transforms init from a hardcoded process spawner into a data-driven
  service manager. The `ServiceDef` format is intentionally simple (shell-variable
  syntax) to keep parsing tractable in `no_std` Rust.
- Several design patterns are inspired by rustysd (MIT-licensed,
  https://github.com/KillingSpark/rustysd): the PID table for O(1) child-to-service
  lookup on SIGCHLD, the explicit `ServiceStatus` state machine enum, the
  `max_restart` cap (rustysd's `max_deaths`) to prevent infinite crash loops, and
  the iterative reverse-dependency shutdown algorithm. These patterns were chosen
  because they are simple, correct, and portable to `no_std` Rust.
- syslogd uses the Phase 39 Unix domain socket infrastructure. The `/dev/log`
  path follows the standard Unix convention. Datagram sockets may be simpler
  than stream sockets for this use case (one message per send).
- crond reuses `clock_gettime(CLOCK_REALTIME)` from Phase 34 for scheduling.
  The sleep-until-next-job approach avoids busy-waiting. Time resolution is
  one minute (matching cron's granularity).
- The `service` command communicates with init via a simple file-based protocol
  (e.g., writing to `/var/run/init.cmd` and signaling init with SIGUSR1) or via
  direct signal sends (SIGTERM to service PID for stop). The exact IPC mechanism
  should be decided during implementation.
- The kernel shutdown path must sync the ext2 filesystem before halting. This
  is critical to prevent data loss on the persistent partition.
- `who`/`w`/`last` depend on login session tracking. If Phase 27's login does
  not write utmp/wtmp records, those must be added as part of this phase.
- The learning doc for Phase 46 should cover: init system design philosophy
  (SysV vs runit vs systemd vs rustysd), daemon process model (why double-fork
  exists), syslog architecture, cron scheduling algorithms, and the PID table /
  state machine patterns borrowed from rustysd.
