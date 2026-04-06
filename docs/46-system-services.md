# System Services

**Aligned Roadmap Phase:** Phase 46
**Status:** Complete
**Source Ref:** phase-46
**Supersedes Legacy Doc:** (none — new content)

## Overview

Phase 46 transforms m3OS from a system that hardcodes which daemons to run into
one that reads service definitions from configuration files, starts them in
dependency order, restarts them on failure, logs their output centrally, schedules
recurring tasks, and shuts down cleanly. This is what separates a hobby kernel
from an administrable server.

## What This Doc Covers

- Init system design: how PID 1 evolves from a hardcoded spawner to a service manager
- Service definition format and dependency graphs
- PID table dispatch for O(1) child-to-service lookup on SIGCHLD
- ServiceStatus state machine and restart policies
- Syslog architecture: Unix domain socket, log formatting, centralized storage
- Cron scheduling: crontab parsing, next-run-time computation, sleep-and-execute
- Kernel shutdown path: sys_reboot, filesystem sync, CPU halt/restart

## Core Implementation

### Service Manager (Enhanced Init)

The key architectural change is replacing hardcoded `spawn_telnetd()` /
`spawn_sshd()` calls with a data-driven approach. Init reads `.conf` files from
`/etc/services.d/`, each declaring a service's name, command, type, restart
policy, and dependencies.

**Service definition format** (`/etc/services.d/sshd.conf`):
```
name=sshd
command=/bin/sshd
type=daemon
restart=always
max_restart=10
depends=syslogd
```

The format is intentionally simple — `key=value` lines, one per field — to keep
parsing tractable in `no_std` Rust with no heap allocator.

**Dependency graph**: Init builds a graph with bidirectional edges. Forward edges
(`depends`) come from the config files; reverse edges (`required_by`) are derived
automatically. Startup walks forward (start dependencies first); shutdown walks
backward (stop dependents first).

**PID table** (inspired by rustysd, MIT): A fixed-size array maps child PIDs to
service indices. When SIGCHLD fires and `waitpid(-1, WNOHANG)` reaps a child,
the PID table provides O(1) lookup to find which service owns that PID. Without
this, init would need to scan all services linearly on every child exit.

**ServiceStatus state machine** (inspired by rustysd, MIT):
```
NeverStarted → Starting → Running → Stopping → Stopped(exit_code)
                                                    ↓
                                              PermanentlyStopped
```
Each transition is driven by a process event (SIGCHLD, fork success) or operator
command (service start/stop). The `Stopped` variant carries the exit code so the
restart policy can distinguish clean exits from crashes.

**Restart cap** (`max_restart`, inspired by rustysd's `max_deaths`, MIT): A
crashing service is restarted up to `max_restart` times (default 10). After that,
it transitions to `PermanentlyStopped` and is no longer restarted. This prevents
a broken binary from consuming all system resources in a crash loop.

**Reverse-dependency shutdown**: Rather than pre-computing a reverse topological
sort, shutdown iteratively finds a running service whose `required_by` dependents
are all already stopped, stops it (SIGTERM, then SIGKILL after timeout), and
repeats. This is O(n²) but simple and correct.

### System Logging (syslogd)

syslogd binds a Unix domain socket (AF_UNIX, SOCK_DGRAM) at `/dev/log` — the
standard Unix syslog path. Services send log messages as datagrams, optionally
prefixed with `<priority>` (RFC 3164 format). syslogd:

1. Receives the datagram
2. Parses the optional `<priority>` prefix
3. Gets the current wall-clock time via `clock_gettime(CLOCK_REALTIME)`
4. Formats: `YYYY-MM-DD HH:MM:SS hostname tag: message`
5. Appends to `/var/log/messages`

Kernel messages are separately drained and written to `/var/log/kern.log`.

The `logger` command provides a shell interface to syslog: it opens a SOCK_DGRAM
socket, formats a message with the given tag and priority, and sends it to
`/dev/log`.

### Cron Scheduling (crond)

crond reads crontab files at startup and enters a sleep loop:

1. Get current time via `clock_gettime(CLOCK_REALTIME)`
2. Convert epoch seconds to broken-down time (year/month/day/hour/minute/weekday)
3. Check each cron entry: does it match the current minute?
4. For matching entries: `fork()` + `execve()` the command
5. Sleep until the next minute boundary
6. Repeat

**Crontab format**: `minute hour day month weekday command`
- Each time field supports: exact numbers (`30`), wildcards (`*`), ranges (`1-5`),
  step values (`*/5`)
- Special strings: `@reboot` (run once at daemon start), `@hourly` (`0 * * * *`),
  `@daily` (`0 0 * * *`)

crond handles SIGHUP to reload crontab files without restart, allowing the
`crontab` command to edit files and signal the daemon to pick up changes.

### Kernel Shutdown Path

The `sys_reboot()` syscall (number 169, matching Linux) accepts a command:
- `REBOOT_CMD_HALT` / `REBOOT_CMD_POWER_OFF`: sync filesystems, halt CPU
- `REBOOT_CMD_RESTART`: sync filesystems, triple-fault CPU reset

Only UID 0 can invoke it. The `shutdown` and `reboot` commands signal init
(SIGTERM) to stop all services, wait briefly, then call `sys_reboot()`.

## Key Files

| File | Purpose |
|---|---|
| `userspace/init/src/main.rs` | PID 1 service manager: parser, dep graph, lifecycle, restart, shutdown |
| `userspace/syslogd/src/main.rs` | System logging daemon: /dev/log socket, log formatting, file output |
| `userspace/crond/src/main.rs` | Cron daemon: crontab parser, scheduler, job executor |
| `userspace/coreutils-rs/src/service.rs` | `service` command: list, status, start, stop, restart |
| `userspace/coreutils-rs/src/logger.rs` | `logger` command: send messages to syslog |
| `userspace/coreutils-rs/src/shutdown.rs` | `shutdown` command: halt the system |
| `userspace/coreutils-rs/src/reboot_cmd.rs` | `reboot` command: restart the system |
| `userspace/coreutils-rs/src/hostname.rs` | `hostname` command: get/set hostname |
| `userspace/coreutils-rs/src/who.rs` | `who` command: show logged-in users |
| `userspace/coreutils-rs/src/last.rs` | `last` command: show login history |
| `userspace/coreutils-rs/src/crontab.rs` | `crontab` command: manage user crontabs |
| `kernel/src/arch/x86_64/syscall.rs` | sys_reboot syscall + kernel_shutdown helper |
| `userspace/syscall-lib/src/lib.rs` | reboot() wrapper, signal constants |
| `kernel/initrd/etc/services.d/*.conf` | Service definition files |

## How This Phase Differs From Later Work

- This phase introduces basic service management with simple `.conf` files.
  A later phase could add systemd-compatible unit files or socket activation.
- Syslog writes to a single file. A later phase could add facility-based routing,
  log rotation, or remote syslog (UDP to a log server).
- Cron uses minute-granularity scheduling. A later phase could add second-granularity
  timers, randomized delay, or access control lists.
- The kernel shutdown path does a basic filesystem sync. A later phase could add
  ACPI power management for real hardware shutdown.

## Related Roadmap Docs

- [Phase 46 roadmap doc](./roadmap/46-system-services.md)
- [Phase 46 task doc](./roadmap/tasks/46-system-services-tasks.md)

## Deferred or Later-Phase Topics

- Socket activation (systemd-style)
- sd_notify readiness protocol
- Service sandboxing (cgroups, namespaces)
- Structured logging (journal)
- Log rotation and compression
- Remote syslog (UDP/TCP)
- NTP time synchronization
- Runlevels and targets
