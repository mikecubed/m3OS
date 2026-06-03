//! Ramdisk filesystem backend — Phase 8 / Phase 18.
//!
//! Embeds a fixed set of files at compile time organised into a hierarchical
//! directory tree ([`RamdiskNode`]).  Public helpers [`ramdisk_lookup`] and
//! [`ramdisk_list_dir`] allow path-based navigation of the tree, while
//! [`get_file`] provides backward-compatible bare-name lookup.
//!
//! The legacy IPC handler ([`handle`]) is retained for the `fat_server` task
//! and uses a private flat file table for index-based file descriptors.
//!
//! No mutable state — the ramdisk is purely read-only.

#![allow(dead_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::protocol::{
    FILE_CLOSE, FILE_LIST, FILE_OPEN, FILE_READ, MAX_LIST_LEN, MAX_NAME_LEN, MAX_READ_LEN,
};
use crate::ipc::Message;

// ===========================================================================
// Directory tree
// ===========================================================================

/// A node in the ramdisk directory tree.
pub enum RamdiskNode {
    /// A regular file with static content embedded at compile time.
    File { content: &'static [u8] },
    /// A directory whose children are `(name, node)` pairs.
    Dir {
        children: &'static [(&'static str, RamdiskNode)],
    },
}

impl RamdiskNode {
    /// Returns `true` if this node is a directory.
    pub fn is_dir(&self) -> bool {
        matches!(self, RamdiskNode::Dir { .. })
    }

    /// Returns `true` if this node is a regular file.
    pub fn is_file(&self) -> bool {
        matches!(self, RamdiskNode::File { .. })
    }
}

// ---------------------------------------------------------------------------
// File payloads — each include_bytes! appears exactly once.
// ---------------------------------------------------------------------------

macro_rules! static_initrd_asset {
    ($path:literal) => {
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/initrd/", $path))
    };
}

macro_rules! generated_initrd_asset {
    ($path:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/generated-initrd/",
            $path
        ))
    };
}

static HELLO_TXT: &[u8] = static_initrd_asset!("hello.txt");
static README_TXT: &[u8] = static_initrd_asset!("readme.txt");
static EXIT0_ELF: &[u8] = generated_initrd_asset!("exit0");
static FORK_TEST_ELF: &[u8] = generated_initrd_asset!("fork-test");
static ECHO_ARGS_ELF: &[u8] = generated_initrd_asset!("echo-args");
static HELLO_ELF: &[u8] = generated_initrd_asset!("hello");
static TMPFS_TEST_ELF: &[u8] = generated_initrd_asset!("tmpfs-test");
static ECHO_ELF: &[u8] = generated_initrd_asset!("echo");
static TRUE_ELF: &[u8] = generated_initrd_asset!("true");
static FALSE_ELF: &[u8] = generated_initrd_asset!("false");
static CAT_ELF: &[u8] = generated_initrd_asset!("cat");
static LS_ELF: &[u8] = generated_initrd_asset!("ls");
static PWD_ELF: &[u8] = generated_initrd_asset!("pwd");
static MKDIR_ELF: &[u8] = generated_initrd_asset!("mkdir");
static RMDIR_ELF: &[u8] = generated_initrd_asset!("rmdir");
static RM_ELF: &[u8] = generated_initrd_asset!("rm");
static CP_ELF: &[u8] = generated_initrd_asset!("cp");
static MV_ELF: &[u8] = generated_initrd_asset!("mv");
static ENV_ELF: &[u8] = generated_initrd_asset!("env");
static SLEEP_ELF: &[u8] = generated_initrd_asset!("sleep");
static GREP_ELF: &[u8] = generated_initrd_asset!("grep");
static SIGNAL_TEST_ELF: &[u8] = generated_initrd_asset!("signal-test");
static PROMPT_ELF: &[u8] = generated_initrd_asset!("PROMPT");
static STDIN_TEST_ELF: &[u8] = generated_initrd_asset!("stdin-test");
static INIT_ELF: &[u8] = generated_initrd_asset!("init");
static SH0_ELF: &[u8] = generated_initrd_asset!("sh0");
static ION_ELF: &[u8] = generated_initrd_asset!("ion");
static EDIT_ELF: &[u8] = generated_initrd_asset!("edit");
static LOGIN_ELF: &[u8] = generated_initrd_asset!("login");
static SU_ELF: &[u8] = generated_initrd_asset!("su");
static PASSWD_ELF: &[u8] = generated_initrd_asset!("passwd");
static ADDUSER_ELF: &[u8] = generated_initrd_asset!("adduser");
static ID_ELF: &[u8] = generated_initrd_asset!("id");
static WHOAMI_ELF: &[u8] = generated_initrd_asset!("whoami");
static KTRACE_ELF: &[u8] = generated_initrd_asset!("ktrace");
static TELNETD_ELF: &[u8] = generated_initrd_asset!("telnetd");
// Phase 43: SSH server
static SSHD_ELF: &[u8] = generated_initrd_asset!("sshd");
// Phase 32: build tools and utilities
static TOUCH_ELF: &[u8] = generated_initrd_asset!("touch");
static STAT_ELF: &[u8] = generated_initrd_asset!("stat");
static LN_ELF: &[u8] = generated_initrd_asset!("ln");
static READLINK_ELF: &[u8] = generated_initrd_asset!("readlink");
static WC_ELF: &[u8] = generated_initrd_asset!("wc");
static AR_ELF: &[u8] = generated_initrd_asset!("ar");
static INSTALL_ELF: &[u8] = generated_initrd_asset!("install");
static MEMINFO_ELF: &[u8] = generated_initrd_asset!("meminfo");
static MMAP_LEAK_TEST_ELF: &[u8] = generated_initrd_asset!("mmap-leak-test");
// Phase 77 Track C: multi-threaded __thread / PT_TLS smoke test.
static TLS_SMOKE_ELF: &[u8] = generated_initrd_asset!("tls-smoke");
// Phase 77 Track D.1: DNS resolution smoke test (musl resolver).
static DNS_SMOKE_ELF: &[u8] = generated_initrd_asset!("dns-smoke");
static MAKE_ELF: &[u8] = generated_initrd_asset!("make");
static HEAD_ELF: &[u8] = generated_initrd_asset!("head");
static TAIL_ELF: &[u8] = generated_initrd_asset!("tail");
static TEE_ELF: &[u8] = generated_initrd_asset!("tee");
static CHMOD_ELF: &[u8] = generated_initrd_asset!("chmod");
static CHOWN_ELF: &[u8] = generated_initrd_asset!("chown");
static SORT_ELF: &[u8] = generated_initrd_asset!("sort");
static UNIQ_ELF: &[u8] = generated_initrd_asset!("uniq");
static CUT_ELF: &[u8] = generated_initrd_asset!("cut");
static TR_ELF: &[u8] = generated_initrd_asset!("tr");
static SED_ELF: &[u8] = generated_initrd_asset!("sed");
static FILE_ELF: &[u8] = generated_initrd_asset!("file");
static HEXDUMP_ELF: &[u8] = generated_initrd_asset!("hexdump");
static DU_ELF: &[u8] = generated_initrd_asset!("du");
static DF_ELF: &[u8] = generated_initrd_asset!("df");
static FIND_ELF: &[u8] = generated_initrd_asset!("find");
static XARGS_ELF: &[u8] = generated_initrd_asset!("xargs");
static FREE_ELF: &[u8] = generated_initrd_asset!("free");
static DMESG_ELF: &[u8] = generated_initrd_asset!("dmesg");
static MOUNT_ELF: &[u8] = generated_initrd_asset!("mount");
static UMOUNT_ELF: &[u8] = generated_initrd_asset!("umount");
static KILL_ELF: &[u8] = generated_initrd_asset!("kill");
static PS_ELF: &[u8] = generated_initrd_asset!("ps");
static STRINGS_ELF: &[u8] = generated_initrd_asset!("strings");
static CAL_ELF: &[u8] = generated_initrd_asset!("cal");
static DIFF_ELF: &[u8] = generated_initrd_asset!("diff");
static PATCH_ELF: &[u8] = generated_initrd_asset!("patch");
static LESS_ELF: &[u8] = generated_initrd_asset!("less");
// Phase 23: ping
static PING_ELF: &[u8] = generated_initrd_asset!("ping");
static SMOKE_RUNNER_ELF: &[u8] = generated_initrd_asset!("smoke-runner");
// Phase 29: PTY test
static PTY_TEST_ELF: &[u8] = generated_initrd_asset!("pty-test");
// Phase 34: timekeeping utilities
static DATE_ELF: &[u8] = generated_initrd_asset!("date");
static UPTIME_ELF: &[u8] = generated_initrd_asset!("uptime");
// Phase 39: Unix domain socket test
static UNIX_SOCKET_TEST_ELF: &[u8] = generated_initrd_asset!("unix-socket-test");
// Phase 40: threading test
static THREAD_TEST_ELF: &[u8] = generated_initrd_asset!("thread-test");
// Phase 42: crypto primitives
static CRYPTO_TEST_ELF: &[u8] = generated_initrd_asset!("crypto-test");
static SHA256SUM_ELF: &[u8] = generated_initrd_asset!("sha256sum");
static GENKEY_ELF: &[u8] = generated_initrd_asset!("genkey");
// Phase 46: system service/operator commands
static SERVICE_ELF: &[u8] = generated_initrd_asset!("service");
static LOGGER_ELF: &[u8] = generated_initrd_asset!("logger");
static SHUTDOWN_ELF: &[u8] = generated_initrd_asset!("shutdown");
static REBOOT_ELF: &[u8] = generated_initrd_asset!("reboot");
static HOSTNAME_ELF: &[u8] = generated_initrd_asset!("hostname");
static WHO_ELF: &[u8] = generated_initrd_asset!("who");
static W_ELF: &[u8] = generated_initrd_asset!("w");
static LAST_ELF: &[u8] = generated_initrd_asset!("last");
static CRONTAB_ELF: &[u8] = generated_initrd_asset!("crontab");
// Phase 44: musl-linked Rust std programs
static HELLO_RUST_ELF: &[u8] = generated_initrd_asset!("hello-rust");
static SYSINFO_RUST_ELF: &[u8] = generated_initrd_asset!("sysinfo-rust");
static HTTPD_RUST_ELF: &[u8] = generated_initrd_asset!("httpd-rust");
static CALC_RUST_ELF: &[u8] = generated_initrd_asset!("calc-rust");
static TODO_RUST_ELF: &[u8] = generated_initrd_asset!("todo-rust");
// Phase 46: background daemons managed by init
static SYSLOGD_ELF: &[u8] = generated_initrd_asset!("syslogd");
static CROND_ELF: &[u8] = generated_initrd_asset!("crond");
// Phase 52: ring-3 extracted services
static CONSOLE_SERVER_ELF: &[u8] = generated_initrd_asset!("console_server");
static KBD_SERVER_ELF: &[u8] = generated_initrd_asset!("kbd_server");
// Phase 56 Track D.2: ring-3 mouse service. PS/2 AUX (IRQ 12) producer of
// PointerEvent messages on the `mouse` IPC service.
static MOUSE_SERVER_ELF: &[u8] = generated_initrd_asset!("mouse_server");
static STDIN_FEEDER_ELF: &[u8] = generated_initrd_asset!("stdin_feeder");
static FAT_SERVER_ELF: &[u8] = generated_initrd_asset!("fat_server");
static VFS_SERVER_ELF: &[u8] = generated_initrd_asset!("vfs_server");
static NET_SERVER_ELF: &[u8] = generated_initrd_asset!("net_server");
// Phase 47: DOOM binary
static DOOM_BIN: &[u8] = generated_initrd_asset!("doom");
// Phase 55b Tracks D.1 / E.1: ring-3 driver scaffolds. Exposed under
// `/drivers/<name>` so init (F.1) and `execve` can find them at the
// canonical driver path.
static NVME_DRIVER_ELF: &[u8] = generated_initrd_asset!("nvme_driver");
static E1000_DRIVER_ELF: &[u8] = generated_initrd_asset!("e1000_driver");
// Phase 80 Track A.5: ring-3 AC'97 out-of-process audio hardware driver.
static AC97_DRIVER_ELF: &[u8] = generated_initrd_asset!("ac97_driver");
// Phase 80b: ring-3 Intel HDA out-of-process audio hardware driver.
static HDA_DRIVER_ELF: &[u8] = generated_initrd_asset!("hda_driver");
// Phase 79: ring-3 modern NIC drivers (Intel e1000e/igb/igc, Realtek r8169/r8125).
static E1000E_DRIVER_ELF: &[u8] = generated_initrd_asset!("e1000e_driver");
static IGB_DRIVER_ELF: &[u8] = generated_initrd_asset!("igb_driver");
static IGC_DRIVER_ELF: &[u8] = generated_initrd_asset!("igc_driver");
static R8169_DRIVER_ELF: &[u8] = generated_initrd_asset!("r8169_driver");
static R8125_DRIVER_ELF: &[u8] = generated_initrd_asset!("r8125_driver");
// Phase 81: ring-3 MediaTek mt792x Wi-Fi driver.
static MT792X_DRIVER_ELF: &[u8] = generated_initrd_asset!("mt792x_driver");
// Phase 78a Track B.2: ring-3 xHCI USB host-controller driver.
static XHCI_DRIVER_ELF: &[u8] = generated_initrd_asset!("xhci_driver");
// Phase 78b Track B: ring-3 USB hub class driver.
static USBHUB_ELF: &[u8] = generated_initrd_asset!("usbhub");
/// Phase 78c: ring-3 USB HID Boot-Protocol class driver (keyboard + mouse).
static USB_HID_ELF: &[u8] = generated_initrd_asset!("usb_hid");
// Phase 55b Track F.3b: NVMe crash-and-restart end-to-end smoke client.
// Exposed under /bin so the QEMU regression can launch it from the shell.
static NVME_CRASH_SMOKE_ELF: &[u8] = generated_initrd_asset!("nvme-crash-smoke");
// Phase 55b Track F.3d-1: max_restart 6-kill loop smoke client.
static MAX_RESTART_SMOKE_ELF: &[u8] = generated_initrd_asset!("max-restart-smoke");
// Phase 55b Track F.3d-3: e1000 crash-and-restart end-to-end smoke client.
static E1000_CRASH_SMOKE_ELF: &[u8] = generated_initrd_asset!("e1000-crash-smoke");
// Phase 56 Track C.1: ring-3 display server (compositor) scaffold.
static DISPLAY_SERVER_ELF: &[u8] = generated_initrd_asset!("display_server");
// Phase 56 Track C.6: protocol-reference client (visual smoke).
static GFX_DEMO_ELF: &[u8] = generated_initrd_asset!("gfx-demo");
// Phase 56 Track E.4: minimal control-socket client. One-shot CLI;
// invoked from the shell or via test harnesses (no `.conf` because it
// is not a daemon — the four-step new-binary convention only requires
// a service config for daemons).
static M3CTL_ELF: &[u8] = generated_initrd_asset!("m3ctl");
// Phase 56 Track F.2: display-service crash-and-restart smoke client.
// Exposed under /bin so the QEMU regression can launch it from the
// post-login shell. No `.conf` (not a daemon).
static DISPLAY_SERVER_CRASH_SMOKE_ELF: &[u8] =
    generated_initrd_asset!("display-server-crash-smoke");

// Phase 56 close-out (G.1): multi-client coexistence smoke client.
// One-shot binary; no `.conf` (launched from the post-login shell by
// the QEMU regression).
static DISPLAY_MULTI_CLIENT_SMOKE_ELF: &[u8] =
    generated_initrd_asset!("display-multi-client-smoke");

// Phase 56 close-out (G.2): keybind grab-hook smoke client.
static GRAB_HOOK_SMOKE_ELF: &[u8] = generated_initrd_asset!("grab-hook-smoke");

// Phase 57 Track F.2: session_manager daemon — graphical-session
// orchestrator. Drives display_server → kbd_server → mouse_server →
// audio_server → term in declared order via
// kernel_core::session::StartupSequence.
static SESSION_MANAGER_ELF: &[u8] = generated_initrd_asset!("session_manager");

// Phase 57 Track D.1 / Phase 63 driver-host correctness fix: audio_server
// daemon — ring-3 AC'97 driver. Exposed under `/drivers/<name>` (not
// `/bin/`) because the kernel's `is_authorized_driver_process` gate keys
// on the `/drivers/` exec-path prefix to authorize `sys_device_claim`.
// Without this prefix, audio_server falls back to stub mode and never
// claims the AC'97 PCI device — leaving `frames_consumed` at zero and
// breaking the Phase 63 audio-smoke gate.
static AUDIO_SERVER_ELF: &[u8] = generated_initrd_asset!("audio_server");

// Phase 57 Track E.2: audio-demo one-shot — generates a 440 Hz sine
// wave and submits it through `audio_client`. Exposed under /bin so
// it is reachable via the shell and via the H.1 smoke harness.
// Intentionally not registered as a service (one-shot, not daemon).
static AUDIO_DEMO_ELF: &[u8] = generated_initrd_asset!("audio-demo");

// Phase 63 Track E.1: audio-stats one-shot — queries audio_server via
// ControlCommand(GetStats) and prints AUDIO_STATS:consumed=<N> underruns=<M>
// followed by AUDIO_STATS:PASS or AUDIO_STATS:FAIL:consumed=0.  General-purpose
// stats query tool. Not a daemon — no .conf entry.
static AUDIO_STATS_ELF: &[u8] = generated_initrd_asset!("audio-stats");

// Phase 63 Track E.1: bell-test one-shot — exercises the Bell::ring →
// AudioClientBellSink → audio_client::submit_frames path directly from sh0,
// bypassing the kbd_server routing gap. Prints BELL_TEST:consumed=<N>
// underruns=<M> followed by BELL_TEST:PASS (frames_consumed > 0) or
// BELL_TEST:FAIL:consumed=0.  Used by the bell-smoke harness.
// Not a daemon — no .conf entry.
static BELL_TEST_ELF: &[u8] = generated_initrd_asset!("bell-test");

// Phase 57 Track G: term — graphical terminal emulator. Exposed under
// /bin so `session_manager` (and `init` via `term.conf`) can launch it
// via the standard service-config path (`command=/bin/term`).
static TERM_ELF: &[u8] = generated_initrd_asset!("term");

// Phase 57d follow-up — Tier 1 fullscreen-takeover wrapper. Exposed
// under /bin so `term` (and the post-login shell) can run
// `fb-takeover /bin/doom`. Not a daemon: no `.conf`.
static FB_TAKEOVER_ELF: &[u8] = generated_initrd_asset!("fb-takeover");

// Phase 64 Track A.3 — deterministic test child for the
// session_manager lifecycle integration tests (B.1 grace-period and
// C.1 crash-loop / display-critical text-fallback paths). Not a
// daemon: no `.conf`.
static CRASH_STUB_ELF: &[u8] = generated_initrd_asset!("crash_stub");

// Phase 69 Track H — `tui-smoke` byte-level validator. Run from the
// post-login shell by `cargo xtask tui-smoke`. Not a daemon: no
// `.conf`.
static TUI_SMOKE_ELF: &[u8] = generated_initrd_asset!("tui-smoke");

// Phase 69a Track I — `tcsmoke` byte-level validator for the POSIX
// termios contract.  Run from the post-login shell by
// `cargo xtask termios-smoke`.  Not a daemon: no `.conf`.
static TCSMOKE_ELF: &[u8] = generated_initrd_asset!("tcsmoke");

// Phase 69d follow-up — `winsize-bang` issues TIOCSWINSZ on the
// controlling TTY.  Run by the htop branch of `tui-app-smoke` to
// drive a SIGWINCH mid-htop.  Not a daemon: no `.conf`.
static WINSIZE_BANG_ELF: &[u8] = generated_initrd_asset!("winsize-bang");

// Phase 69d follow-up — `sendmsg-test` regression: SCM_RIGHTS over
// AF_UNIX SOCK_STREAM.  Run from the post-login shell by `cargo xtask
// tui-app-smoke` ahead of the tmux session lifecycle (and ad hoc).
// Not a daemon: no `.conf`.
static SENDMSG_TEST_ELF: &[u8] = generated_initrd_asset!("sendmsg-test");

// Phase 74 Track B.3 — `page-grant-test` round-trip regression: brk
// 1024 pages, sentinel-fill, sys_page_grant_send → CapHandle,
// sys_page_grant_recv → fresh vaddr, verify sentinel survives,
// assert double-recv fails. Not a daemon: no `.conf`.
static PAGE_GRANT_TEST_ELF: &[u8] = generated_initrd_asset!("page-grant-test");

// Phase 75 Track G.1 — `wx-violation` W^X-enforcement regression:
// mmap RW, assert `mprotect(PROT_WRITE | PROT_EXEC)` is rejected with
// EINVAL by the new `sys_mprotect` guard, then assert the supported
// JIT pattern `mprotect(PROT_READ | PROT_EXEC)` succeeds.  Not a daemon:
// no `.conf`.
static WX_VIOLATION_ELF: &[u8] = generated_initrd_asset!("wx-violation");

// Phase 77 Track F.1 — `epoll-smoke` epoll_* verification regression.
static EPOLL_SMOKE_ELF: &[u8] = generated_initrd_asset!("epoll-smoke");

// Phase 76 — `dynlink_smoke` musl-built dynamic ELF carrying
// `PT_INTERP = /lib/ld-musl-x86_64.so.1` and zero `DT_NEEDED`
// entries. The smoke-runner execs this to validate the kernel
// `PT_INTERP` branch + the ld.so transfer-only stub end to end.
// `include_bytes!` accepts a zero-byte placeholder gracefully (the
// smoke runner detects size==0 and emits SKIP at runtime), so this
// definition is always present even when the host lacks a C
// compiler.
static DYNLINK_SMOKE_ELF: &[u8] = generated_initrd_asset!("dynlink_smoke");

// Phase 76b — `dynlink_hello` musl-built dynamic ELF carrying
// `PT_INTERP = /lib/ld-musl-x86_64.so.1` AND `DT_NEEDED = libhello.so`.
// The smoke-runner execs `/bin/dynlink_hello` to validate the full
// PT_INTERP → ld.so self-relocation → DT_NEEDED → relocations →
// main → external symbol call chain.
static DYNLINK_HELLO_ELF: &[u8] = generated_initrd_asset!("dynlink_hello");

// Phase 76b F1.4 — `dynlink_missing` musl-built dynamic ELF with
// `DT_NEEDED = libdoesnotexist.so`. Smoke gate asserts the linker
// exits with code 2 (ENOENT).
static DYNLINK_MISSING_ELF: &[u8] = generated_initrd_asset!("dynlink_missing");

// Phase 76b F1.4 — `dynlink_cycle` musl-built dynamic ELF whose
// DT_NEEDED chain has a libcyca ↔ libcycb cycle. Smoke gate asserts
// the linker exits with code 80 (ELIBBAD).
static DYNLINK_CYCLE_ELF: &[u8] = generated_initrd_asset!("dynlink_cycle");

// Phase 76c — `dlopen_test` musl-built dynamic ELF with PT_INTERP +
// DT_NEEDED libdl.so. Exercises the full dlopen / dlsym / dlclose /
// dlerror lifecycle including DT_FINI_ARRAY destructors on
// `libhello_fini.so`. Drives the dlopen-test-smoke gate.
static DLOPEN_TEST_ELF: &[u8] = generated_initrd_asset!("dlopen_test");

// Phase 76d.F — `dynlink_hello_gnu` musl-built dynamic ELF with
// PT_INTERP + DT_NEEDED libhello_gnu.so, both built with
// `--hash-style=gnu`. Exercises Phase 76d.D1's GNU hash backend end
// to end via the bring-up linker plus B4's lazy PLT resolve. Also
// emits BIND_NOW:{0,1} (resolution mode) and WX_CHECK:OK (F.4 W^X
// invariant) sentinels for the dynlink-hello-gnu-smoke gate.
static DYNLINK_HELLO_GNU_ELF: &[u8] = generated_initrd_asset!("dynlink_hello_gnu");

// Phase 76d.G — `dynlink_hello_versioned` musl-built dynamic ELF
// with PT_INTERP + DT_NEEDED libhello_versioned.so. The lib's
// `--version-script` exports `hello_str` under symbol version
// `LIBHELLO_1.0`; the consumer's `DT_VERNEED` carries the matching
// requirement. Exercises Phase 76d.D2.2 version-aware lookup end to
// end via the dynlink-hello-versioned-smoke gate.
static DYNLINK_HELLO_VERSIONED_ELF: &[u8] = generated_initrd_asset!("dynlink_hello_versioned");

// Phase 76d.G.3 — `dynlink_hello_versioned_mismatch`. Linked against
// a stub lib that defines `hello_str@LIBHELLO_1.0`, runs at boot
// against the REAL v2 lib that only defines `hello_str@LIBHELLO_2.0`
// plus an unversioned `hello_str`. Drives the mismatch-fallback +
// LD_BIND_NOW strict-mode gates.
static DYNLINK_HELLO_VERSIONED_MISMATCH_ELF: &[u8] =
    generated_initrd_asset!("dynlink_hello_versioned_mismatch");

// Phase 70 follow-up — `doom-concurrent` forks two `doom` processes
// and waits for both. Run from the post-login shell by `cargo xtask
// doom-concurrent-smoke` to assert real kernel-level concurrency
// (the in-tree shell has no `&` job control or `wait` builtin, so
// the previous shell-driven gate degraded to a sequential run).
// Not a daemon: no `.conf`.
static DOOM_CONCURRENT_ELF: &[u8] = generated_initrd_asset!("doom-concurrent");

// Phase 71 — `greeter` GUI login manager. Display-server client that
// authenticates against /etc/passwd + /etc/shadow, then in-process
// `setuid` + `execve(/bin/term)` so term inherits the authenticated
// UID/GID. Exposed under /bin so init can spawn it directly from the
// `/etc/services.d/greeter.conf` manifest (graphical-only boots only);
// `session_manager` observes readiness via the IPC registry rather
// than parenting the process.
static GREETER_ELF: &[u8] = generated_initrd_asset!("greeter");

// Phase 73 — desktop background Layer-shell client.
static WALLPAPER_ELF: &[u8] = generated_initrd_asset!("wallpaper");

// Phase 73 — persistent status bar Layer-shell client.
static BAR_ELF: &[u8] = generated_initrd_asset!("bar");

// Phase 73 — SUPER+SPACE fuzzy-filter launcher (Toplevel).
static LAUNCHER_ELF: &[u8] = generated_initrd_asset!("launcher");

// Phase 73 — notification daemon (AF_UNIX listener) and companion
// `notify-send` CLI.
static NOTIFYD_ELF: &[u8] = generated_initrd_asset!("notifyd");
static NOTIFY_SEND_ELF: &[u8] = generated_initrd_asset!("notify-send");

// Phase 73 — lockscreen Layer-shell stub (exclusive keyboard grab).
static LOCKSCREEN_ELF: &[u8] = generated_initrd_asset!("lockscreen");

// ---------------------------------------------------------------------------
// Static tree construction (separate statics to work around const-eval limits)
// ---------------------------------------------------------------------------

static BIN_ENTRIES: &[(&str, RamdiskNode)] = &[
    ("exit0", RamdiskNode::File { content: EXIT0_ELF }),
    (
        "fork-test",
        RamdiskNode::File {
            content: FORK_TEST_ELF,
        },
    ),
    (
        "echo-args",
        RamdiskNode::File {
            content: ECHO_ARGS_ELF,
        },
    ),
    ("hello", RamdiskNode::File { content: HELLO_ELF }),
    (
        "smoke-runner",
        RamdiskNode::File {
            content: SMOKE_RUNNER_ELF,
        },
    ),
    (
        "tmpfs-test",
        RamdiskNode::File {
            content: TMPFS_TEST_ELF,
        },
    ),
    ("echo", RamdiskNode::File { content: ECHO_ELF }),
    ("true", RamdiskNode::File { content: TRUE_ELF }),
    ("false", RamdiskNode::File { content: FALSE_ELF }),
    ("cat", RamdiskNode::File { content: CAT_ELF }),
    ("ls", RamdiskNode::File { content: LS_ELF }),
    ("pwd", RamdiskNode::File { content: PWD_ELF }),
    ("mkdir", RamdiskNode::File { content: MKDIR_ELF }),
    ("rmdir", RamdiskNode::File { content: RMDIR_ELF }),
    ("rm", RamdiskNode::File { content: RM_ELF }),
    ("cp", RamdiskNode::File { content: CP_ELF }),
    ("mv", RamdiskNode::File { content: MV_ELF }),
    ("env", RamdiskNode::File { content: ENV_ELF }),
    ("sleep", RamdiskNode::File { content: SLEEP_ELF }),
    ("grep", RamdiskNode::File { content: GREP_ELF }),
    (
        "signal-test",
        RamdiskNode::File {
            content: SIGNAL_TEST_ELF,
        },
    ),
    (
        "PROMPT",
        RamdiskNode::File {
            content: PROMPT_ELF,
        },
    ),
    (
        "stdin-test",
        RamdiskNode::File {
            content: STDIN_TEST_ELF,
        },
    ),
    ("sh0", RamdiskNode::File { content: SH0_ELF }),
    ("ion", RamdiskNode::File { content: ION_ELF }),
    // Phase 32: /bin/sh alias for ion (pdpmake and scripts expect /bin/sh)
    ("sh", RamdiskNode::File { content: ION_ELF }),
    ("edit", RamdiskNode::File { content: EDIT_ELF }),
    ("login", RamdiskNode::File { content: LOGIN_ELF }),
    ("su", RamdiskNode::File { content: SU_ELF }),
    (
        "passwd",
        RamdiskNode::File {
            content: PASSWD_ELF,
        },
    ),
    (
        "adduser",
        RamdiskNode::File {
            content: ADDUSER_ELF,
        },
    ),
    ("id", RamdiskNode::File { content: ID_ELF }),
    (
        "whoami",
        RamdiskNode::File {
            content: WHOAMI_ELF,
        },
    ),
    (
        "ktrace",
        RamdiskNode::File {
            content: KTRACE_ELF,
        },
    ),
    (
        "telnetd",
        RamdiskNode::File {
            content: TELNETD_ELF,
        },
    ),
    // Phase 43: SSH server
    ("sshd", RamdiskNode::File { content: SSHD_ELF }),
    (
        "syslogd",
        RamdiskNode::File {
            content: SYSLOGD_ELF,
        },
    ),
    ("crond", RamdiskNode::File { content: CROND_ELF }),
    // Phase 52: ring-3 extracted services
    (
        "console_server",
        RamdiskNode::File {
            content: CONSOLE_SERVER_ELF,
        },
    ),
    (
        "kbd_server",
        RamdiskNode::File {
            content: KBD_SERVER_ELF,
        },
    ),
    // Phase 56 Track D.2: ring-3 mouse service.
    (
        "mouse_server",
        RamdiskNode::File {
            content: MOUSE_SERVER_ELF,
        },
    ),
    (
        "stdin_feeder",
        RamdiskNode::File {
            content: STDIN_FEEDER_ELF,
        },
    ),
    (
        "fat_server",
        RamdiskNode::File {
            content: FAT_SERVER_ELF,
        },
    ),
    (
        "vfs_server",
        RamdiskNode::File {
            content: VFS_SERVER_ELF,
        },
    ),
    // Phase 54 Track C: ring-3 UDP network service
    (
        "net_server",
        RamdiskNode::File {
            content: NET_SERVER_ELF,
        },
    ),
    // Phase 56 Track C.1: ring-3 display server (compositor) scaffold.
    (
        "display_server",
        RamdiskNode::File {
            content: DISPLAY_SERVER_ELF,
        },
    ),
    // Phase 57 Track F.2: session_manager daemon — graphical-session
    // orchestrator.
    (
        "session_manager",
        RamdiskNode::File {
            content: SESSION_MANAGER_ELF,
        },
    ),
    // Phase 57 Track E.2: audio-demo one-shot reference client.
    (
        "audio-demo",
        RamdiskNode::File {
            content: AUDIO_DEMO_ELF,
        },
    ),
    // Phase 63 Track E.1: audio-stats one-shot stats CLI.
    (
        "audio-stats",
        RamdiskNode::File {
            content: AUDIO_STATS_ELF,
        },
    ),
    // Phase 63 Track E.1: bell-test — bell path exerciser for the bell-smoke harness.
    (
        "bell-test",
        RamdiskNode::File {
            content: BELL_TEST_ELF,
        },
    ),
    // Phase 57 Track G: term — graphical terminal emulator (the first
    // non-demo display_server client).
    ("term", RamdiskNode::File { content: TERM_ELF }),
    // Phase 69 Track H: tui-smoke — byte-level validator for the
    // m3os-term terminal contract.
    (
        "tui-smoke",
        RamdiskNode::File {
            content: TUI_SMOKE_ELF,
        },
    ),
    // Phase 69a Track I: tcsmoke — byte-level validator for the POSIX
    // termios contract.
    (
        "tcsmoke",
        RamdiskNode::File {
            content: TCSMOKE_ELF,
        },
    ),
    // Phase 69d follow-up: winsize-bang — issues TIOCSWINSZ for the
    // tui-app-smoke htop SIGWINCH reflow assertion.
    (
        "winsize-bang",
        RamdiskNode::File {
            content: WINSIZE_BANG_ELF,
        },
    ),
    // Phase 69d follow-up: sendmsg-test — SCM_RIGHTS regression.
    (
        "sendmsg-test",
        RamdiskNode::File {
            content: SENDMSG_TEST_ELF,
        },
    ),
    // Phase 74 Track B.3: page-grant-test — round-trip regression.
    (
        "page-grant-test",
        RamdiskNode::File {
            content: PAGE_GRANT_TEST_ELF,
        },
    ),
    // Phase 75 Track G.1: wx-violation — W^X enforcement regression.
    (
        "wx-violation",
        RamdiskNode::File {
            content: WX_VIOLATION_ELF,
        },
    ),
    // Phase 77 Track F.1: epoll-smoke — epoll_* verification regression.
    (
        "epoll-smoke",
        RamdiskNode::File {
            content: EPOLL_SMOKE_ELF,
        },
    ),
    // Phase 76: dynlink_smoke — kernel PT_INTERP + ld.so transfer
    // smoke. Drives the dynlink-smoke step in the SMOKE: gate.
    (
        "dynlink_smoke",
        RamdiskNode::File {
            content: DYNLINK_SMOKE_ELF,
        },
    ),
    // Phase 76b: dynlink_hello — exercises DT_NEEDED + relocations
    // against `/usr/lib/libhello.so`. Drives the dynlink-hello-smoke
    // step in the SMOKE: gate.
    (
        "dynlink_hello",
        RamdiskNode::File {
            content: DYNLINK_HELLO_ELF,
        },
    ),
    // Phase 76b F1.4 negative gates.
    (
        "dynlink_missing",
        RamdiskNode::File {
            content: DYNLINK_MISSING_ELF,
        },
    ),
    (
        "dynlink_cycle",
        RamdiskNode::File {
            content: DYNLINK_CYCLE_ELF,
        },
    ),
    // Phase 76d.F: dynlink_hello_gnu — exercises GNU-hash backend +
    // PLT lazy resolve end-to-end. Drives the dynlink-hello-gnu-smoke
    // gate.
    (
        "dynlink_hello_gnu",
        RamdiskNode::File {
            content: DYNLINK_HELLO_GNU_ELF,
        },
    ),
    // Phase 76d.G: dynlink_hello_versioned — exercises version-aware
    // lookup end-to-end. Drives the dynlink-hello-versioned-smoke
    // gate.
    (
        "dynlink_hello_versioned",
        RamdiskNode::File {
            content: DYNLINK_HELLO_VERSIONED_ELF,
        },
    ),
    // Phase 76d.G.3: dynlink_hello_versioned_mismatch — drives the
    // mismatch-fallback + LD_BIND_NOW strict-mode gates.
    (
        "dynlink_hello_versioned_mismatch",
        RamdiskNode::File {
            content: DYNLINK_HELLO_VERSIONED_MISMATCH_ELF,
        },
    ),
    // Phase 76c: dlopen_test — exercises dlopen / dlsym / dlclose /
    // dlerror via `libdl.so` (resolved through the dynamic linker's
    // self-injected scope). Drives the dlopen-test-smoke step.
    (
        "dlopen_test",
        RamdiskNode::File {
            content: DLOPEN_TEST_ELF,
        },
    ),
    // Phase 70 follow-up: doom-concurrent — forks two doom children
    // and waits for both. Drives the doom-concurrent-smoke gate.
    (
        "doom-concurrent",
        RamdiskNode::File {
            content: DOOM_CONCURRENT_ELF,
        },
    ),
    // Phase 32: build tools and utilities
    ("touch", RamdiskNode::File { content: TOUCH_ELF }),
    ("stat", RamdiskNode::File { content: STAT_ELF }),
    ("ln", RamdiskNode::File { content: LN_ELF }),
    (
        "readlink",
        RamdiskNode::File {
            content: READLINK_ELF,
        },
    ),
    ("wc", RamdiskNode::File { content: WC_ELF }),
    ("ar", RamdiskNode::File { content: AR_ELF }),
    (
        "install",
        RamdiskNode::File {
            content: INSTALL_ELF,
        },
    ),
    (
        "meminfo",
        RamdiskNode::File {
            content: MEMINFO_ELF,
        },
    ),
    ("head", RamdiskNode::File { content: HEAD_ELF }),
    ("tail", RamdiskNode::File { content: TAIL_ELF }),
    ("tee", RamdiskNode::File { content: TEE_ELF }),
    ("chmod", RamdiskNode::File { content: CHMOD_ELF }),
    ("chown", RamdiskNode::File { content: CHOWN_ELF }),
    ("sort", RamdiskNode::File { content: SORT_ELF }),
    ("uniq", RamdiskNode::File { content: UNIQ_ELF }),
    ("cut", RamdiskNode::File { content: CUT_ELF }),
    ("tr", RamdiskNode::File { content: TR_ELF }),
    ("sed", RamdiskNode::File { content: SED_ELF }),
    ("file", RamdiskNode::File { content: FILE_ELF }),
    (
        "hexdump",
        RamdiskNode::File {
            content: HEXDUMP_ELF,
        },
    ),
    ("du", RamdiskNode::File { content: DU_ELF }),
    ("df", RamdiskNode::File { content: DF_ELF }),
    ("find", RamdiskNode::File { content: FIND_ELF }),
    ("xargs", RamdiskNode::File { content: XARGS_ELF }),
    ("free", RamdiskNode::File { content: FREE_ELF }),
    ("dmesg", RamdiskNode::File { content: DMESG_ELF }),
    ("mount", RamdiskNode::File { content: MOUNT_ELF }),
    (
        "umount",
        RamdiskNode::File {
            content: UMOUNT_ELF,
        },
    ),
    ("kill", RamdiskNode::File { content: KILL_ELF }),
    ("ps", RamdiskNode::File { content: PS_ELF }),
    (
        "strings",
        RamdiskNode::File {
            content: STRINGS_ELF,
        },
    ),
    ("cal", RamdiskNode::File { content: CAL_ELF }),
    ("diff", RamdiskNode::File { content: DIFF_ELF }),
    ("patch", RamdiskNode::File { content: PATCH_ELF }),
    ("less", RamdiskNode::File { content: LESS_ELF }),
    ("make", RamdiskNode::File { content: MAKE_ELF }),
    ("ping", RamdiskNode::File { content: PING_ELF }),
    (
        "pty-test",
        RamdiskNode::File {
            content: PTY_TEST_ELF,
        },
    ),
    // Phase 33: mmap/munmap leak test
    (
        "mmap-leak-test",
        RamdiskNode::File {
            content: MMAP_LEAK_TEST_ELF,
        },
    ),
    // Phase 77 Track C: multi-threaded __thread / PT_TLS smoke test
    (
        "tls-smoke",
        RamdiskNode::File {
            content: TLS_SMOKE_ELF,
        },
    ),
    // Phase 77 Track D.1: DNS resolution smoke test
    (
        "dns-smoke",
        RamdiskNode::File {
            content: DNS_SMOKE_ELF,
        },
    ),
    // Phase 34: timekeeping utilities
    ("date", RamdiskNode::File { content: DATE_ELF }),
    (
        "uptime",
        RamdiskNode::File {
            content: UPTIME_ELF,
        },
    ),
    // Phase 39: Unix domain socket test
    (
        "unix-socket-test",
        RamdiskNode::File {
            content: UNIX_SOCKET_TEST_ELF,
        },
    ),
    // Phase 40: threading test
    (
        "thread-test",
        RamdiskNode::File {
            content: THREAD_TEST_ELF,
        },
    ),
    // Phase 42: crypto primitives
    (
        "crypto-test",
        RamdiskNode::File {
            content: CRYPTO_TEST_ELF,
        },
    ),
    (
        "sha256sum",
        RamdiskNode::File {
            content: SHA256SUM_ELF,
        },
    ),
    (
        "genkey",
        RamdiskNode::File {
            content: GENKEY_ELF,
        },
    ),
    // Phase 46: system service/operator commands
    (
        "service",
        RamdiskNode::File {
            content: SERVICE_ELF,
        },
    ),
    (
        "logger",
        RamdiskNode::File {
            content: LOGGER_ELF,
        },
    ),
    (
        "shutdown",
        RamdiskNode::File {
            content: SHUTDOWN_ELF,
        },
    ),
    (
        "reboot",
        RamdiskNode::File {
            content: REBOOT_ELF,
        },
    ),
    (
        "hostname",
        RamdiskNode::File {
            content: HOSTNAME_ELF,
        },
    ),
    ("who", RamdiskNode::File { content: WHO_ELF }),
    ("w", RamdiskNode::File { content: W_ELF }),
    ("last", RamdiskNode::File { content: LAST_ELF }),
    (
        "crontab",
        RamdiskNode::File {
            content: CRONTAB_ELF,
        },
    ),
    // Phase 44: musl-linked Rust std programs
    (
        "hello-rust",
        RamdiskNode::File {
            content: HELLO_RUST_ELF,
        },
    ),
    (
        "sysinfo-rust",
        RamdiskNode::File {
            content: SYSINFO_RUST_ELF,
        },
    ),
    (
        "httpd-rust",
        RamdiskNode::File {
            content: HTTPD_RUST_ELF,
        },
    ),
    (
        "calc-rust",
        RamdiskNode::File {
            content: CALC_RUST_ELF,
        },
    ),
    (
        "todo-rust",
        RamdiskNode::File {
            content: TODO_RUST_ELF,
        },
    ),
    // Phase 47: DOOM
    ("doom", RamdiskNode::File { content: DOOM_BIN }),
    // Phase 55b Track F.3b: NVMe crash-and-restart smoke client.
    (
        "nvme-crash-smoke",
        RamdiskNode::File {
            content: NVME_CRASH_SMOKE_ELF,
        },
    ),
    // Phase 55b Track F.3d-1: max_restart 6-kill loop smoke client.
    (
        "max-restart-smoke",
        RamdiskNode::File {
            content: MAX_RESTART_SMOKE_ELF,
        },
    ),
    // Phase 55b Track F.3d-3: e1000 crash-and-restart smoke client.
    (
        "e1000-crash-smoke",
        RamdiskNode::File {
            content: E1000_CRASH_SMOKE_ELF,
        },
    ),
    // Phase 56 Track C.6: protocol-reference client (visual smoke).
    (
        "gfx-demo",
        RamdiskNode::File {
            content: GFX_DEMO_ELF,
        },
    ),
    // Phase 56 Track E.4: minimal control-socket CLI. Not a daemon
    // — invoked by the shell or test harness; no `.conf` required.
    ("m3ctl", RamdiskNode::File { content: M3CTL_ELF }),
    // Phase 57d follow-up — Tier 1 fullscreen-takeover wrapper.
    // Invoked manually from the shell (`fb-takeover /bin/doom`) so
    // doom can paint directly to the framebuffer between
    // `display_server` yield and reclaim. Not a daemon.
    (
        "fb-takeover",
        RamdiskNode::File {
            content: FB_TAKEOVER_ELF,
        },
    ),
    // Phase 64 Track A.3 — deterministic test child for the
    // session_manager lifecycle integration tests. Not a daemon;
    // launched from the test harness with one of three modes
    // (exit-immediately, ignore-sigterm, exit-on-sigterm).
    (
        "crash_stub",
        RamdiskNode::File {
            content: CRASH_STUB_ELF,
        },
    ),
    // Phase 56 Track F.2: display-service crash-and-restart smoke
    // client. Not a daemon; invoked from the post-login shell by the
    // F.2 regression. No `.conf` required.
    (
        "display-server-crash-smoke",
        RamdiskNode::File {
            content: DISPLAY_SERVER_CRASH_SMOKE_ELF,
        },
    ),
    // Phase 56 close-out (G.1): multi-client coexistence smoke client.
    // One-shot; launched from the post-login shell by the
    // multi-client-coexistence regression.
    (
        "display-multi-client-smoke",
        RamdiskNode::File {
            content: DISPLAY_MULTI_CLIENT_SMOKE_ELF,
        },
    ),
    // Phase 56 close-out (G.2): keybind grab-hook smoke client.
    (
        "grab-hook-smoke",
        RamdiskNode::File {
            content: GRAB_HOOK_SMOKE_ELF,
        },
    ),
    // Phase 71 — `greeter` GUI login manager. Spawned by `init` via
    // `/etc/services.d/greeter.conf` in graphical-only boots (marker
    // `/etc/m3os-graphical-only` present); `session_manager` only
    // observes readiness via the IPC registry and is not greeter's
    // parent. On successful authentication greeter `setuid`s to the
    // authenticated user and `execve`s `/bin/term` in-process.
    (
        "greeter",
        RamdiskNode::File {
            content: GREETER_ELF,
        },
    ),
    // Phase 73 — desktop background client.
    (
        "wallpaper",
        RamdiskNode::File {
            content: WALLPAPER_ELF,
        },
    ),
    // Phase 73 — status bar client.
    ("bar", RamdiskNode::File { content: BAR_ELF }),
    // Phase 73 — fuzzy-filter app launcher.
    (
        "launcher",
        RamdiskNode::File {
            content: LAUNCHER_ELF,
        },
    ),
    // Phase 73 — notification daemon + CLI.
    (
        "notifyd",
        RamdiskNode::File {
            content: NOTIFYD_ELF,
        },
    ),
    (
        "notify-send",
        RamdiskNode::File {
            content: NOTIFY_SEND_ELF,
        },
    ),
    // Phase 73 — lockscreen stub.
    (
        "lockscreen",
        RamdiskNode::File {
            content: LOCKSCREEN_ELF,
        },
    ),
];

static ETC_ENTRIES: &[(&str, RamdiskNode)] = &[
    ("hello.txt", RamdiskNode::File { content: HELLO_TXT }),
    (
        "readme.txt",
        RamdiskNode::File {
            content: README_TXT,
        },
    ),
];

static SBIN_ENTRIES: &[(&str, RamdiskNode)] = &[("init", RamdiskNode::File { content: INIT_ELF })];

// Phase 76 — dynamic linker (`ld-musl-x86_64.so.1`). Staged in both
// the ramdisk and the ext2 `/lib/` so the kernel's `PT_INTERP` branch
// can resolve the path before ext2 is mounted (early-boot smoke).
//
// The macro path uses dots in the filename (`ld-musl-x86_64.so.1`),
// matching the on-disk staging produced by `xtask::build_ldso`. The
// generated-libs directory is a sibling of generated-initrd; we reach
// it via `../generated-libs/` because the existing
// `generated_initrd_asset!` macro is keyed off generated-initrd.
static LDSO_ELF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../target/generated-libs/ld-musl-x86_64.so.1"
));

// Phase 76b — libhello.so embedded in the ramdisk so the bring-up
// linker can resolve `/usr/lib/libhello.so` even before the ext2
// data disk mounts. The build path mirrors LDSO_ELF.
static LIBHELLO_ELF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../target/generated-libs/libhello.so"
));

// Phase 76c — libdl.so link-time stub; libhello_fini.so destructor
// demo. Both embedded the same way so dlopen_test resolves its
// DT_NEEDED libdl.so and runtime-`dlopen` of libhello_fini.so against
// `/usr/lib/` even before the ext2 disk mounts.
static LIBDL_ELF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../target/generated-libs/libdl.so"
));
static LIBHELLO_FINI_ELF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../target/generated-libs/libhello_fini.so"
));

static LIB_ENTRIES: &[(&str, RamdiskNode)] = &[(
    "ld-musl-x86_64.so.1",
    RamdiskNode::File { content: LDSO_ELF },
)];

// Phase 76b — `/usr/lib/libhello.so` lives in its own directory entry
// because the ramdisk tree mirrors the on-disk layout. Adding a new
// LIB_ENTRIES slot would map to `/lib/`, which is wrong; the linker's
// search order finds libhello under `/usr/lib/` first.
static USR_LIB_ENTRIES: &[(&str, RamdiskNode)] = &[
    (
        "libhello.so",
        RamdiskNode::File {
            content: LIBHELLO_ELF,
        },
    ),
    ("libdl.so", RamdiskNode::File { content: LIBDL_ELF }),
    (
        "libhello_fini.so",
        RamdiskNode::File {
            content: LIBHELLO_FINI_ELF,
        },
    ),
];

static USR_ENTRIES: &[(&str, RamdiskNode)] = &[(
    "lib",
    RamdiskNode::Dir {
        children: USR_LIB_ENTRIES,
    },
)];

// Phase 55b Tracks D.1 / E.1 — hardware driver ELFs. Ring-3 drivers live
// under `/drivers/<name>` so init's service registration (Track F.1) and
// any future `execve` call can target a canonical path that is not mixed
// in with general userspace utilities under `/bin/`.
static DRIVERS_ENTRIES: &[(&str, RamdiskNode)] = &[
    (
        "nvme",
        RamdiskNode::File {
            content: NVME_DRIVER_ELF,
        },
    ),
    (
        "e1000",
        RamdiskNode::File {
            content: E1000_DRIVER_ELF,
        },
    ),
    // Phase 79: ring-3 modern NIC drivers (Intel e1000e/igb/igc, Realtek r8169/r8125).
    (
        "e1000e",
        RamdiskNode::File {
            content: E1000E_DRIVER_ELF,
        },
    ),
    (
        "igb",
        RamdiskNode::File {
            content: IGB_DRIVER_ELF,
        },
    ),
    (
        "igc",
        RamdiskNode::File {
            content: IGC_DRIVER_ELF,
        },
    ),
    (
        "r8169",
        RamdiskNode::File {
            content: R8169_DRIVER_ELF,
        },
    ),
    (
        "r8125",
        RamdiskNode::File {
            content: R8125_DRIVER_ELF,
        },
    ),
    // Phase 81: ring-3 MediaTek mt792x Wi-Fi driver → /drivers/mt792x.
    (
        "mt792x",
        RamdiskNode::File {
            content: MT792X_DRIVER_ELF,
        },
    ),
    // Phase 63 driver-host fix: audio_server is a ring-3 driver and must
    // live under `/drivers/` so `is_authorized_driver_process` accepts its
    // `sys_device_claim(0,0,5,0)` call for the AC'97 controller.
    (
        "audio_server",
        RamdiskNode::File {
            content: AUDIO_SERVER_ELF,
        },
    ),
    // Phase 80 Track A.5: ring-3 AC'97 out-of-process audio hardware driver.
    // Lives under `/drivers/` so `is_authorized_driver_process` accepts its
    // `sys_device_claim(0,0,5,0)` call for the AC'97 controller.
    (
        "ac97_driver",
        RamdiskNode::File {
            content: AC97_DRIVER_ELF,
        },
    ),
    // Phase 80b: ring-3 Intel HDA driver. Lives under `/drivers/` so
    // `is_authorized_driver_process` accepts its `sys_device_claim` for the
    // class-0x0403 HDA controller.
    (
        "hda_driver",
        RamdiskNode::File {
            content: HDA_DRIVER_ELF,
        },
    ),
    // Phase 78a Track B.2: ring-3 xHCI driver. Lives under `/drivers/` so
    // `is_authorized_driver_process` accepts its `sys_device_claim` for the
    // qemu-xhci controller.
    (
        "xhci",
        RamdiskNode::File {
            content: XHCI_DRIVER_ELF,
        },
    ),
    // Phase 78b Track B: ring-3 USB hub class driver.
    (
        "usbhub",
        RamdiskNode::File {
            content: USBHUB_ELF,
        },
    ),
    // Phase 78c: ring-3 USB HID class driver, staged under /drivers/ so the
    // is_authorized_driver_process gate admits it.
    (
        "usb-hid",
        RamdiskNode::File {
            content: USB_HID_ELF,
        },
    ),
];

static ROOT_ENTRIES: &[(&str, RamdiskNode)] = &[
    (
        "bin",
        RamdiskNode::Dir {
            children: BIN_ENTRIES,
        },
    ),
    (
        "sbin",
        RamdiskNode::Dir {
            children: SBIN_ENTRIES,
        },
    ),
    (
        "etc",
        RamdiskNode::Dir {
            children: ETC_ENTRIES,
        },
    ),
    (
        "drivers",
        RamdiskNode::Dir {
            children: DRIVERS_ENTRIES,
        },
    ),
    // Phase 76 — `/lib/ld-musl-x86_64.so.1` reachable via the ramdisk
    // path. The kernel's PT_INTERP reader tries the ramdisk first
    // before falling back to the ext2 mount.
    (
        "lib",
        RamdiskNode::Dir {
            children: LIB_ENTRIES,
        },
    ),
    // Phase 76b — `/usr/lib/libhello.so` reachable before the ext2
    // mount.
    (
        "usr",
        RamdiskNode::Dir {
            children: USR_ENTRIES,
        },
    ),
];

/// The root of the ramdisk directory tree.
static RAMDISK_ROOT: RamdiskNode = RamdiskNode::Dir {
    children: ROOT_ENTRIES,
};

// ===========================================================================
// Tree navigation helpers
// ===========================================================================

/// Look up a node by path in the ramdisk tree.
///
/// Accepts both absolute (`/bin/cat`) and relative (`bin/cat`) paths;
/// leading slashes are stripped before traversal. An empty path returns root.
///
/// # Examples
///
/// ```ignore
/// ramdisk_lookup("/")              // → root Dir
/// ramdisk_lookup("/bin")           // → bin Dir
/// ramdisk_lookup("/bin/cat")       // → File
/// ramdisk_lookup("/etc/hello.txt") // → File
/// ```
pub fn ramdisk_lookup(path: &str) -> Option<&'static RamdiskNode> {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Some(&RAMDISK_ROOT);
    }

    let mut current = &RAMDISK_ROOT;
    for component in trimmed.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        match current {
            RamdiskNode::Dir { children } => {
                match children.iter().find(|(name, _)| *name == component) {
                    Some((_, node)) => current = node,
                    None => return None,
                }
            }
            RamdiskNode::File { .. } => return None,
        }
    }
    Some(current)
}

/// List children of a ramdisk directory.
///
/// Returns `(name, is_dir)` pairs, or `None` if the path does not refer to a
/// directory.
pub fn ramdisk_list_dir(path: &str) -> Option<Vec<(String, bool)>> {
    let node = ramdisk_lookup(path)?;
    match node {
        RamdiskNode::Dir { children } => {
            let mut result = Vec::new();
            for (name, child) in children.iter() {
                result.push((String::from(*name), child.is_dir()));
            }
            Some(result)
        }
        RamdiskNode::File { .. } => None,
    }
}

// ===========================================================================
// Public file access (used by syscalls)
// ===========================================================================

/// Look up a file by path and return a reference to its static content.
///
/// Accepts paths with or without a leading `/`.  For backward compatibility a
/// bare filename such as `"cat"` is searched under `/bin/` and then
/// `/etc/`.
///
/// Used by `sys_open`, `sys_execve`, and `resolve_command`.
pub fn get_file(name: &str) -> Option<&'static [u8]> {
    // Try exact path first — avoid allocation when already absolute.
    if name.starts_with('/') {
        if let Some(RamdiskNode::File { content }) = ramdisk_lookup(name) {
            return Some(content);
        }
    } else {
        let path = alloc::format!("/{}", name);
        if let Some(RamdiskNode::File { content }) = ramdisk_lookup(&path) {
            return Some(content);
        }
    }

    // Backward compatibility: try under /bin/ and /etc/ for bare filenames.
    if !name.contains('/') {
        let bin_path = alloc::format!("/bin/{}", name);
        if let Some(RamdiskNode::File { content }) = ramdisk_lookup(&bin_path) {
            return Some(content);
        }
        let etc_path = alloc::format!("/etc/{}", name);
        if let Some(RamdiskNode::File { content }) = ramdisk_lookup(&etc_path) {
            return Some(content);
        }
    }

    None
}

// ===========================================================================
// Legacy flat file table (for IPC backward compatibility)
// ===========================================================================

/// Private flat entry used by the IPC `handle_open` / `handle_read` path.
struct FlatFile {
    name: &'static str,
    content: &'static [u8],
}

/// Flat file array preserving the original index-based fd scheme expected by
/// `fs_client_task` and the VFS IPC protocol.  References the same named
/// statics as the directory tree — no duplicate `include_bytes!`.
static FLAT_FILES: &[FlatFile] = &[
    FlatFile {
        name: "hello.txt",
        content: HELLO_TXT,
    },
    FlatFile {
        name: "readme.txt",
        content: README_TXT,
    },
    FlatFile {
        name: "exit0",
        content: EXIT0_ELF,
    },
    FlatFile {
        name: "fork-test",
        content: FORK_TEST_ELF,
    },
    FlatFile {
        name: "echo-args",
        content: ECHO_ARGS_ELF,
    },
    FlatFile {
        name: "hello",
        content: HELLO_ELF,
    },
    FlatFile {
        name: "tmpfs-test",
        content: TMPFS_TEST_ELF,
    },
    FlatFile {
        name: "echo",
        content: ECHO_ELF,
    },
    FlatFile {
        name: "true",
        content: TRUE_ELF,
    },
    FlatFile {
        name: "false",
        content: FALSE_ELF,
    },
    FlatFile {
        name: "cat",
        content: CAT_ELF,
    },
    FlatFile {
        name: "ls",
        content: LS_ELF,
    },
    FlatFile {
        name: "pwd",
        content: PWD_ELF,
    },
    FlatFile {
        name: "mkdir",
        content: MKDIR_ELF,
    },
    FlatFile {
        name: "rmdir",
        content: RMDIR_ELF,
    },
    FlatFile {
        name: "rm",
        content: RM_ELF,
    },
    FlatFile {
        name: "cp",
        content: CP_ELF,
    },
    FlatFile {
        name: "mv",
        content: MV_ELF,
    },
    FlatFile {
        name: "env",
        content: ENV_ELF,
    },
    FlatFile {
        name: "sleep",
        content: SLEEP_ELF,
    },
    FlatFile {
        name: "grep",
        content: GREP_ELF,
    },
    FlatFile {
        name: "ln",
        content: LN_ELF,
    },
    FlatFile {
        name: "readlink",
        content: READLINK_ELF,
    },
    FlatFile {
        name: "head",
        content: HEAD_ELF,
    },
    FlatFile {
        name: "tail",
        content: TAIL_ELF,
    },
    FlatFile {
        name: "tee",
        content: TEE_ELF,
    },
    FlatFile {
        name: "chmod",
        content: CHMOD_ELF,
    },
    FlatFile {
        name: "chown",
        content: CHOWN_ELF,
    },
    FlatFile {
        name: "sort",
        content: SORT_ELF,
    },
    FlatFile {
        name: "uniq",
        content: UNIQ_ELF,
    },
    FlatFile {
        name: "cut",
        content: CUT_ELF,
    },
    FlatFile {
        name: "tr",
        content: TR_ELF,
    },
    FlatFile {
        name: "sed",
        content: SED_ELF,
    },
    FlatFile {
        name: "file",
        content: FILE_ELF,
    },
    FlatFile {
        name: "hexdump",
        content: HEXDUMP_ELF,
    },
    FlatFile {
        name: "du",
        content: DU_ELF,
    },
    FlatFile {
        name: "df",
        content: DF_ELF,
    },
    FlatFile {
        name: "find",
        content: FIND_ELF,
    },
    FlatFile {
        name: "xargs",
        content: XARGS_ELF,
    },
    FlatFile {
        name: "free",
        content: FREE_ELF,
    },
    FlatFile {
        name: "dmesg",
        content: DMESG_ELF,
    },
    FlatFile {
        name: "mount",
        content: MOUNT_ELF,
    },
    FlatFile {
        name: "umount",
        content: UMOUNT_ELF,
    },
    FlatFile {
        name: "kill",
        content: KILL_ELF,
    },
    FlatFile {
        name: "ps",
        content: PS_ELF,
    },
    FlatFile {
        name: "strings",
        content: STRINGS_ELF,
    },
    FlatFile {
        name: "cal",
        content: CAL_ELF,
    },
    FlatFile {
        name: "diff",
        content: DIFF_ELF,
    },
    FlatFile {
        name: "patch",
        content: PATCH_ELF,
    },
    FlatFile {
        name: "less",
        content: LESS_ELF,
    },
];

// ---------------------------------------------------------------------------
// Static name list (null-separated, for FILE_LIST)
// ---------------------------------------------------------------------------

const fn file_name_list_len() -> usize {
    let mut total = 0;
    let mut index = 0;
    while index < FLAT_FILES.len() {
        total += FLAT_FILES[index].name.len() + 1;
        index += 1;
    }
    total
}

const FILE_NAME_LIST_LEN: usize = file_name_list_len();
const _: [(); 1] = [(); (FILE_NAME_LIST_LEN <= MAX_LIST_LEN) as usize];

const fn build_file_name_list() -> [u8; FILE_NAME_LIST_LEN] {
    let mut buf = [0; FILE_NAME_LIST_LEN];
    let mut out = 0;
    let mut file_index = 0;
    while file_index < FLAT_FILES.len() {
        let name = FLAT_FILES[file_index].name.as_bytes();
        let mut byte_index = 0;
        while byte_index < name.len() {
            buf[out] = name[byte_index];
            out += 1;
            byte_index += 1;
        }
        buf[out] = 0;
        out += 1;
        file_index += 1;
    }
    buf
}

static FILE_NAME_LIST: [u8; FILE_NAME_LIST_LEN] = build_file_name_list();

fn name_list() -> (*const u8, usize) {
    (FILE_NAME_LIST.as_ptr(), FILE_NAME_LIST.len())
}

// ===========================================================================
// IPC message handler
// ===========================================================================

/// Handle one `fat_server` IPC message and return the reply [`Message`].
///
/// Dispatches on `msg.label`:
/// - [`FILE_OPEN`]  — look up a file by name; reply with its fd or `u64::MAX`.
/// - [`FILE_READ`]  — return a pointer + length into the static content.
/// - [`FILE_CLOSE`] — no-op; reply with an empty ack message.
/// - [`FILE_LIST`]  — return the null-separated name list.
/// - anything else  — reply with label `u64::MAX` (unknown operation).
pub fn handle(msg: &Message) -> Message {
    match msg.label {
        FILE_OPEN => handle_open(msg),
        FILE_READ => handle_read(msg),
        FILE_CLOSE => Message::new(0),
        FILE_LIST => {
            let (ptr, len) = name_list();
            let mut reply = Message::new(0);
            reply.data[0] = ptr as u64;
            reply.data[1] = len as u64;
            reply
        }
        _ => Message::new(u64::MAX),
    }
}

// ---------------------------------------------------------------------------
// FILE_OPEN (IPC — uses flat table for index-based fds)
// ---------------------------------------------------------------------------

fn handle_open(msg: &Message) -> Message {
    let ptr = msg.data[0];
    let len = msg.data[1] as usize;

    if ptr == 0 || len == 0 || len > MAX_NAME_LEN {
        return Message::with1(0, u64::MAX);
    }

    // SAFETY: Phase 8 — all callers are kernel tasks executing in the same
    // address space as the kernel.  `ptr` was constructed by the caller as
    // `name_str.as_ptr() as u64` and `len` as `name_str.len() as u64`, so
    // the memory region [ptr, ptr+len) is a valid, live, UTF-8 string in
    // kernel memory for the duration of this synchronous call.
    let name_bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };

    let name = match core::str::from_utf8(name_bytes) {
        Ok(s) => s,
        Err(_) => return Message::with1(0, u64::MAX),
    };

    for (index, file) in FLAT_FILES.iter().enumerate() {
        if file.name == name {
            return Message::with1(0, index as u64);
        }
    }

    Message::with1(0, u64::MAX)
}

// ---------------------------------------------------------------------------
// FILE_READ (IPC — uses flat table for index-based fds)
// ---------------------------------------------------------------------------

fn handle_read(msg: &Message) -> Message {
    let fd = msg.data[0];
    let offset = msg.data[1] as usize;
    let max_len = msg.data[2] as usize;

    let fd_usize = match usize::try_from(fd) {
        Ok(v) => v,
        Err(_) => return Message::with2(0, 0, 0),
    };
    if fd_usize >= FLAT_FILES.len() {
        return Message::with2(0, 0, 0);
    }

    let file = &FLAT_FILES[fd_usize];

    if offset > file.content.len() {
        return Message::with2(0, 0, 0);
    }

    let available = file.content.len() - offset;
    let actual_len = available.min(max_len).min(MAX_READ_LEN);

    let content_ptr = file.content[offset..].as_ptr() as u64;

    Message::with2(0, content_ptr, actual_len as u64)
}
