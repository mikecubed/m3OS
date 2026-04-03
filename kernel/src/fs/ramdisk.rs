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

static HELLO_TXT: &[u8] = include_bytes!("../../initrd/hello.txt");
static README_TXT: &[u8] = include_bytes!("../../initrd/readme.txt");
static EXIT0_ELF: &[u8] = include_bytes!("../../initrd/exit0");
static FORK_TEST_ELF: &[u8] = include_bytes!("../../initrd/fork-test");
static ECHO_ARGS_ELF: &[u8] = include_bytes!("../../initrd/echo-args");
static HELLO_ELF: &[u8] = include_bytes!("../../initrd/hello");
static TMPFS_TEST_ELF: &[u8] = include_bytes!("../../initrd/tmpfs-test");
static ECHO_ELF: &[u8] = include_bytes!("../../initrd/echo");
static TRUE_ELF: &[u8] = include_bytes!("../../initrd/true");
static FALSE_ELF: &[u8] = include_bytes!("../../initrd/false");
static CAT_ELF: &[u8] = include_bytes!("../../initrd/cat");
static LS_ELF: &[u8] = include_bytes!("../../initrd/ls");
static PWD_ELF: &[u8] = include_bytes!("../../initrd/pwd");
static MKDIR_ELF: &[u8] = include_bytes!("../../initrd/mkdir");
static RMDIR_ELF: &[u8] = include_bytes!("../../initrd/rmdir");
static RM_ELF: &[u8] = include_bytes!("../../initrd/rm");
static CP_ELF: &[u8] = include_bytes!("../../initrd/cp");
static MV_ELF: &[u8] = include_bytes!("../../initrd/mv");
static ENV_ELF: &[u8] = include_bytes!("../../initrd/env");
static SLEEP_ELF: &[u8] = include_bytes!("../../initrd/sleep");
static GREP_ELF: &[u8] = include_bytes!("../../initrd/grep");
static SIGNAL_TEST_ELF: &[u8] = include_bytes!("../../initrd/signal-test");
static PROMPT_ELF: &[u8] = include_bytes!("../../initrd/PROMPT");
static STDIN_TEST_ELF: &[u8] = include_bytes!("../../initrd/stdin-test");
static INIT_ELF: &[u8] = include_bytes!("../../initrd/init");
static SH0_ELF: &[u8] = include_bytes!("../../initrd/sh0");
static ION_ELF: &[u8] = include_bytes!("../../initrd/ion");
static EDIT_ELF: &[u8] = include_bytes!("../../initrd/edit");
static LOGIN_ELF: &[u8] = include_bytes!("../../initrd/login");
static SU_ELF: &[u8] = include_bytes!("../../initrd/su");
static PASSWD_ELF: &[u8] = include_bytes!("../../initrd/passwd");
static ADDUSER_ELF: &[u8] = include_bytes!("../../initrd/adduser");
static ID_ELF: &[u8] = include_bytes!("../../initrd/id");
static WHOAMI_ELF: &[u8] = include_bytes!("../../initrd/whoami");
static TELNETD_ELF: &[u8] = include_bytes!("../../initrd/telnetd");
// Phase 32: build tools and utilities
static TOUCH_ELF: &[u8] = include_bytes!("../../initrd/touch");
static STAT_ELF: &[u8] = include_bytes!("../../initrd/stat");
static LN_ELF: &[u8] = include_bytes!("../../initrd/ln");
static READLINK_ELF: &[u8] = include_bytes!("../../initrd/readlink");
static WC_ELF: &[u8] = include_bytes!("../../initrd/wc");
static AR_ELF: &[u8] = include_bytes!("../../initrd/ar");
static INSTALL_ELF: &[u8] = include_bytes!("../../initrd/install");
static MEMINFO_ELF: &[u8] = include_bytes!("../../initrd/meminfo");
static MMAP_LEAK_TEST_ELF: &[u8] = include_bytes!("../../initrd/mmap-leak-test");
static MAKE_ELF: &[u8] = include_bytes!("../../initrd/make");
static HEAD_ELF: &[u8] = include_bytes!("../../initrd/head");
static TAIL_ELF: &[u8] = include_bytes!("../../initrd/tail");
static TEE_ELF: &[u8] = include_bytes!("../../initrd/tee");
static CHMOD_ELF: &[u8] = include_bytes!("../../initrd/chmod");
static CHOWN_ELF: &[u8] = include_bytes!("../../initrd/chown");
static SORT_ELF: &[u8] = include_bytes!("../../initrd/sort");
static UNIQ_ELF: &[u8] = include_bytes!("../../initrd/uniq");
static CUT_ELF: &[u8] = include_bytes!("../../initrd/cut");
static TR_ELF: &[u8] = include_bytes!("../../initrd/tr");
static SED_ELF: &[u8] = include_bytes!("../../initrd/sed");
static FILE_ELF: &[u8] = include_bytes!("../../initrd/file");
static HEXDUMP_ELF: &[u8] = include_bytes!("../../initrd/hexdump");
static DU_ELF: &[u8] = include_bytes!("../../initrd/du");
static DF_ELF: &[u8] = include_bytes!("../../initrd/df");
static FIND_ELF: &[u8] = include_bytes!("../../initrd/find");
static XARGS_ELF: &[u8] = include_bytes!("../../initrd/xargs");
static FREE_ELF: &[u8] = include_bytes!("../../initrd/free");
static DMESG_ELF: &[u8] = include_bytes!("../../initrd/dmesg");
static MOUNT_ELF: &[u8] = include_bytes!("../../initrd/mount");
static UMOUNT_ELF: &[u8] = include_bytes!("../../initrd/umount");
static KILL_ELF: &[u8] = include_bytes!("../../initrd/kill");
static PS_ELF: &[u8] = include_bytes!("../../initrd/ps");
static STRINGS_ELF: &[u8] = include_bytes!("../../initrd/strings");
static CAL_ELF: &[u8] = include_bytes!("../../initrd/cal");
static DIFF_ELF: &[u8] = include_bytes!("../../initrd/diff");
static PATCH_ELF: &[u8] = include_bytes!("../../initrd/patch");
static LESS_ELF: &[u8] = include_bytes!("../../initrd/less");
// Phase 34: timekeeping utilities
static DATE_ELF: &[u8] = include_bytes!("../../initrd/date");
static UPTIME_ELF: &[u8] = include_bytes!("../../initrd/uptime");
// Phase 40: threading test
static THREAD_TEST_ELF: &[u8] = include_bytes!("../../initrd/thread-test");
// Phase 42: crypto primitives
static CRYPTO_TEST_ELF: &[u8] = include_bytes!("../../initrd/crypto-test");
static SHA256SUM_ELF: &[u8] = include_bytes!("../../initrd/sha256sum");
static GENKEY_ELF: &[u8] = include_bytes!("../../initrd/genkey");

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
        "telnetd",
        RamdiskNode::File {
            content: TELNETD_ELF,
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
    // Phase 33: mmap/munmap leak test
    (
        "mmap-leak-test",
        RamdiskNode::File {
            content: MMAP_LEAK_TEST_ELF,
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
