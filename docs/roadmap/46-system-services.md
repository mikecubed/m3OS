# Phase 46 - System Services

**Status:** Complete
**Source Ref:** phase-46
**Depends on:** Phase 19 (Signals) ✅, Phase 24 (Persistent Storage) ✅, Phase 27 (User Accounts) ✅, Phase 29 (PTY) ✅, Phase 30 (Telnet) ✅, Phase 34 (Real-Time Clock) ✅, Phase 39 (Unix Domain Sockets) ✅, Phase 43 (SSH) ✅
**Builds on:** Enhances the Phase 20 init daemon with service management, adds new userspace daemons (syslogd, crond), and extends the kernel with reboot/shutdown syscalls
**Primary Components:** userspace/init, userspace/syslogd, userspace/crond, userspace/coreutils-rs, kernel/src/arch/x86_64/syscall.rs

## Milestone Goal

The OS has a service manager, system logging, and scheduled task execution. Services
(sshd, telnetd, etc.) are managed by a unified init/service system that handles
startup ordering, restart on failure, and clean shutdown. The OS behaves like a real
server that can be administered remotely.

## Why This Phase Exists

Until now, init hardcodes which services to spawn and has no mechanism for dependency
ordering, automatic restart, or coordinated shutdown. There is no centralized logging
— services write to serial or nowhere. There is no way to schedule recurring tasks.
These are the three pillars of a real server OS: service management, logging, and
task scheduling. Without them, the OS cannot be operated like a production system.

## Learning Goals

- Understand how init systems (SysV init, systemd, runit) manage service lifecycles.
- Learn how syslog provides centralized logging for all system services.
- See how cron enables scheduled automation.
- Understand daemon processes: double-fork, setsid, and why they exist.

## Feature Scope

### Service Manager (enhanced init)

Extend the existing PID 1 init to manage services:

**Service Definition** (`/etc/services.d/sshd.conf`):
```
name=sshd
command=/sbin/sshd
type=daemon
restart=always
max_restart=10
depends=network
```

**Service Operations:**
- `service start <name>` — start a service
- `service stop <name>` — stop a service (send SIGTERM, then SIGKILL)
- `service restart <name>` — stop then start
- `service status <name>` — show running/stopped, PID, uptime
- `service list` — show all services and their status

**Lifecycle Management:**
- Start services in dependency order at boot.
- Restart services that exit unexpectedly (if `restart=always`), up to `max_restart` times.
- Handle `SIGCHLD` to detect service exits via a PID table that maps child PIDs to service entries.
- Track service state via a state machine: `NeverStarted -> Starting -> Running -> Stopping -> Stopped`.
- Clean shutdown: iteratively find services with no unstopped dependents and stop them (reverse dependency walk).

### System Logging (`syslogd`)

- Accept log messages from services via a Unix domain socket (`/dev/log`) or
  a simple kernel ring buffer interface.
- Write logs to `/var/log/messages` (persistent) and `/var/log/kern.log` (kernel messages).
- Log format: `timestamp hostname service[pid]: message`
- `logger` command to send log messages from the shell.
- `tail -f /var/log/messages` for live log monitoring.

### Scheduled Tasks (`crond`)

A minimal cron daemon:
- Read `/etc/crontab` and `/var/spool/cron/<user>` crontab files.
- Standard cron format: `minute hour day month weekday command`
- Execute commands at scheduled times.
- Special strings: `@reboot`, `@hourly`, `@daily`
- `crontab -l` to list scheduled jobs and `crontab -r` to remove them.
- Interactive `crontab -e` editing is deferred.

### System Administration Commands

- **`shutdown`** — initiate clean system shutdown (stop services, sync disks, halt)
- **`reboot`** — clean restart
- **`hostname`** — get/set the system hostname
- **`date`** — display/set system date and time (already exists, may need set support)
- **`who` / `w`** — show logged-in users (reads utmp/wtmp or PTY table)
- **`last`** — show recent logins (reads wtmp log)

### Kernel Support

- `reboot()` syscall for orderly system halt and restart.
- Kernel shutdown path: sync filesystems, stop drivers, halt/reboot CPU.
- `clock_gettime` improvements for accurate cron scheduling (if needed).
- Unix domain sockets (for syslog) — already available from Phase 39.

## Important Components and How They Work

### Enhanced Init (Service Manager)

Init (PID 1) currently hardcodes service spawning in `userspace/init/src/main.rs`.
Phase 46 replaces this with a data-driven approach: init reads service definition
files from `/etc/services.d/`, fills bidirectional dependency edges (both `depends`
and `required_by` directions), and starts services in dependency order.

**PID table dispatch** (inspired by rustysd, MIT): Init maintains a `PidTable` —
a map from child PID to `PidEntry` (which service or helper process it belongs to).
On SIGCHLD, `waitpid(-1, WNOHANG)` collects all exited children and the PID table
provides O(1) lookup to the owning service, avoiding a linear scan of all services.

**Service state machine** (inspired by rustysd, MIT): Each service tracks its
lifecycle via a `ServiceStatus` enum: `NeverStarted -> Starting -> Running ->
Stopping -> Stopped`. The `Stopped` variant carries the exit status. Transitions
are driven by process events (SIGCHLD) and operator commands (start/stop).

**Restart cap** (`max_restart`): A `max_restart` field in the service definition
(default 10) limits how many times a crashing service is restarted before init
gives up and marks it permanently stopped. This prevents a broken service from
consuming all resources in a restart loop.

**Reverse-order shutdown** (inspired by rustysd, MIT): Rather than pre-computing
a reverse topological sort, shutdown iteratively finds a running service whose
dependents are all already stopped, stops it, and repeats. This is O(n^2) but
correct and avoids maintaining a separate sorted list.

The `service` command communicates with init through `/var/run/init.cmd`, which
PID 1 polls in its main loop to process start/stop/restart requests.

### syslogd

A userspace daemon that binds a Unix domain socket at `/dev/log` (using AF_UNIX
from Phase 39). Services connect and write log messages in a simple
`<priority>message` format. syslogd reads from the socket, prepends a timestamp and
service identity, and appends the formatted line to `/var/log/messages`. Kernel
messages are read from the existing dmesg ring buffer and written to
`/var/log/kern.log`.

### crond

A userspace daemon that reads crontab files at startup, computes the next execution
time for each entry, and sleeps (via `nanosleep`) until the next job is due. When a
job fires, crond forks and execs the command. It re-reads crontab files when
signaled with SIGHUP. The `crontab` command edits per-user crontab files in
`/var/spool/cron/` and signals crond to reload.

### Kernel Shutdown Path

A new `sys_reboot()` syscall (number 169) accepts a command argument (halt, restart,
power-off). It signals init to begin clean shutdown. Init stops all services in
reverse dependency order, syncs filesystems, then calls `sys_reboot()` again with
a kernel-only flag that performs the actual CPU halt or triple-fault restart.

## How This Builds on Earlier Phases

- Extends Phase 20 (init daemon) by replacing hardcoded service spawning with a
  data-driven service manager that reads `/etc/services.d/` definitions.
- Reuses Phase 19 (signals) for SIGCHLD-based service exit detection, SIGTERM/SIGKILL
  for service stop, and SIGHUP for daemon reload.
- Reuses Phase 39 (Unix domain sockets) for the syslog `/dev/log` socket.
- Reuses Phase 34 (real-time clock) for cron scheduling and log timestamps.
- Reuses Phase 24 (persistent storage) for log files, crontabs, and service definitions.
- Reuses Phase 27 (user accounts) for service ownership and per-user crontabs.
- Manages Phase 30 (telnetd) and Phase 43 (sshd) as declared services rather than
  hardcoded spawns.

## Implementation Outline

1. Design the service definition file format (including `max_restart`) and create `/etc/services.d/` entries.
2. Enhance init to parse service definitions and fill bidirectional dependency edges.
3. Implement dependency-ordered startup (topological sort or iterative readiness check).
4. Implement PID table mapping child PIDs to service entries for O(1) SIGCHLD dispatch.
5. Implement `ServiceStatus` state machine enum (`NeverStarted -> Starting -> Running -> Stopping -> Stopped`).
6. Implement SIGCHLD-based service exit detection with PID table lookup and automatic restart (capped by `max_restart`).
7. Write the `service` command (start, stop, restart, status, list).
8. Write `syslogd`: bind `/dev/log`, accept connections, write to log files.
9. Write the `logger` command.
10. Write `crond`: parse crontab format, compute next run time, sleep-and-execute loop.
11. Write the `crontab` command.
12. Implement `sys_reboot()` syscall and kernel shutdown path.
13. Write `shutdown` and `reboot` commands.
14. Write/update `hostname`, `who`, `w`, `last` utilities.
15. Implement iterative reverse-dependency shutdown (find service with no unstopped dependents, stop it, repeat).
16. Test full lifecycle: boot → services start → cron runs → remote login → shutdown.

## Acceptance Criteria

- Services defined in `/etc/services.d/` start automatically at boot in dependency order.
- `service status sshd` shows the service is running with its PID.
- Killing a `restart=always` service causes it to be automatically restarted.
- A service that crashes more than `max_restart` times is marked permanently stopped.
- `service stop telnetd` cleanly stops the telnet server.
- Log messages from services appear in `/var/log/messages`.
- `logger "test message"` writes to the system log.
- A cron job scheduled for every minute executes on time.
- `shutdown` cleanly stops all services and halts the system.
- `reboot` cleanly restarts the system.
- `who` shows currently logged-in users.

## Companion Task List

- [Phase 46 Task List](./tasks/46-system-services-tasks.md)

## How Real OS Implementations Differ

Real service managers:
- systemd: unit files, socket activation, cgroups, journal, timer units — enormous scope
- rustysd (MIT, Rust): systemd-compatible unit files, socket activation, sd_notify
  readiness protocol, thread-per-event model. Our PID table dispatch, service state
  machine, and reverse-order shutdown patterns are inspired by rustysd's design.
- runit: simple supervision tree, process 1/2/3 stages
- OpenRC: dependency-based init with parallel startup
- launchd (macOS): combines init, cron, inetd, and more

Our approach is closest to runit or early SysV init: simple service definitions,
dependency ordering, and basic supervision. This is enough to understand the concepts
without the complexity of systemd. Key patterns borrowed from rustysd (MIT-licensed):
PID table for O(1) child-to-service lookup, explicit state machine enum for service
lifecycle, max-restart cap to prevent crash loops, and iterative reverse-dependency
shutdown.

Real cron implementations (Vixie cron, dcron) handle:
- Mailto on job output
- Timezone-aware scheduling
- Randomized delay
- Access control (cron.allow, cron.deny)

Our cron is minimal: parse the format, execute at the right time.

Real syslog implementations (rsyslog, syslog-ng) support:
- Remote syslog (UDP/TCP to log servers)
- Facility/priority-based routing to different files
- Log rotation and compression
- Structured logging (RFC 5424)

Our syslog writes to a single file with basic formatting.

## Deferred Until Later

- Socket activation (systemd-style, as in rustysd)
- sd_notify readiness protocol (rustysd supports this; we use timeout-based heuristics)
- Service sandboxing (cgroups, namespaces)
- Journal (structured logging)
- Log rotation
- Remote syslog
- NTP time synchronization
- systemd-compatible unit files
- Runlevels / targets
