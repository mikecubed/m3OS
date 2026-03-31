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
static EXIT0_ELF: &[u8] = include_bytes!("../../initrd/exit0.elf");
static FORK_TEST_ELF: &[u8] = include_bytes!("../../initrd/fork-test.elf");
static ECHO_ARGS_ELF: &[u8] = include_bytes!("../../initrd/echo-args.elf");
static HELLO_ELF: &[u8] = include_bytes!("../../initrd/hello.elf");
static TMPFS_TEST_ELF: &[u8] = include_bytes!("../../initrd/tmpfs-test.elf");
static ECHO_ELF: &[u8] = include_bytes!("../../initrd/echo.elf");
static TRUE_ELF: &[u8] = include_bytes!("../../initrd/true.elf");
static FALSE_ELF: &[u8] = include_bytes!("../../initrd/false.elf");
static CAT_ELF: &[u8] = include_bytes!("../../initrd/cat.elf");
static LS_ELF: &[u8] = include_bytes!("../../initrd/ls.elf");
static PWD_ELF: &[u8] = include_bytes!("../../initrd/pwd.elf");
static MKDIR_ELF: &[u8] = include_bytes!("../../initrd/mkdir.elf");
static RMDIR_ELF: &[u8] = include_bytes!("../../initrd/rmdir.elf");
static RM_ELF: &[u8] = include_bytes!("../../initrd/rm.elf");
static CP_ELF: &[u8] = include_bytes!("../../initrd/cp.elf");
static MV_ELF: &[u8] = include_bytes!("../../initrd/mv.elf");
static ENV_ELF: &[u8] = include_bytes!("../../initrd/env.elf");
static SLEEP_ELF: &[u8] = include_bytes!("../../initrd/sleep.elf");
static GREP_ELF: &[u8] = include_bytes!("../../initrd/grep.elf");
static SIGNAL_TEST_ELF: &[u8] = include_bytes!("../../initrd/signal-test.elf");
static PROMPT_ELF: &[u8] = include_bytes!("../../initrd/PROMPT.elf");
static STDIN_TEST_ELF: &[u8] = include_bytes!("../../initrd/stdin-test.elf");
static INIT_ELF: &[u8] = include_bytes!("../../initrd/init.elf");
static SH0_ELF: &[u8] = include_bytes!("../../initrd/sh0.elf");
static ION_ELF: &[u8] = include_bytes!("../../initrd/ion.elf");
static EDIT_ELF: &[u8] = include_bytes!("../../initrd/edit.elf");
static LOGIN_ELF: &[u8] = include_bytes!("../../initrd/login.elf");
static SU_ELF: &[u8] = include_bytes!("../../initrd/su.elf");
static PASSWD_ELF: &[u8] = include_bytes!("../../initrd/passwd.elf");
static ADDUSER_ELF: &[u8] = include_bytes!("../../initrd/adduser.elf");
static ID_ELF: &[u8] = include_bytes!("../../initrd/id.elf");
static WHOAMI_ELF: &[u8] = include_bytes!("../../initrd/whoami.elf");
static TELNETD_ELF: &[u8] = include_bytes!("../../initrd/telnetd.elf");
// Phase 32: build tools and utilities
static TOUCH_ELF: &[u8] = include_bytes!("../../initrd/touch.elf");
static STAT_ELF: &[u8] = include_bytes!("../../initrd/stat.elf");
static WC_ELF: &[u8] = include_bytes!("../../initrd/wc.elf");
static AR_ELF: &[u8] = include_bytes!("../../initrd/ar.elf");
static INSTALL_ELF: &[u8] = include_bytes!("../../initrd/install.elf");
static MAKE_ELF: &[u8] = include_bytes!("../../initrd/make.elf");

// ---------------------------------------------------------------------------
// Static tree construction (separate statics to work around const-eval limits)
// ---------------------------------------------------------------------------

static BIN_ENTRIES: &[(&str, RamdiskNode)] = &[
    ("exit0.elf", RamdiskNode::File { content: EXIT0_ELF }),
    (
        "fork-test.elf",
        RamdiskNode::File {
            content: FORK_TEST_ELF,
        },
    ),
    (
        "echo-args.elf",
        RamdiskNode::File {
            content: ECHO_ARGS_ELF,
        },
    ),
    ("hello.elf", RamdiskNode::File { content: HELLO_ELF }),
    (
        "tmpfs-test.elf",
        RamdiskNode::File {
            content: TMPFS_TEST_ELF,
        },
    ),
    ("echo.elf", RamdiskNode::File { content: ECHO_ELF }),
    ("true.elf", RamdiskNode::File { content: TRUE_ELF }),
    ("false.elf", RamdiskNode::File { content: FALSE_ELF }),
    ("cat.elf", RamdiskNode::File { content: CAT_ELF }),
    ("ls.elf", RamdiskNode::File { content: LS_ELF }),
    ("pwd.elf", RamdiskNode::File { content: PWD_ELF }),
    ("mkdir.elf", RamdiskNode::File { content: MKDIR_ELF }),
    ("rmdir.elf", RamdiskNode::File { content: RMDIR_ELF }),
    ("rm.elf", RamdiskNode::File { content: RM_ELF }),
    ("cp.elf", RamdiskNode::File { content: CP_ELF }),
    ("mv.elf", RamdiskNode::File { content: MV_ELF }),
    ("env.elf", RamdiskNode::File { content: ENV_ELF }),
    ("sleep.elf", RamdiskNode::File { content: SLEEP_ELF }),
    ("grep.elf", RamdiskNode::File { content: GREP_ELF }),
    (
        "signal-test.elf",
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
        "PROMPT.elf",
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
    (
        "stdin-test.elf",
        RamdiskNode::File {
            content: STDIN_TEST_ELF,
        },
    ),
    ("sh0", RamdiskNode::File { content: SH0_ELF }),
    ("sh0.elf", RamdiskNode::File { content: SH0_ELF }),
    ("ion", RamdiskNode::File { content: ION_ELF }),
    ("ion.elf", RamdiskNode::File { content: ION_ELF }),
    ("edit", RamdiskNode::File { content: EDIT_ELF }),
    ("edit.elf", RamdiskNode::File { content: EDIT_ELF }),
    ("login", RamdiskNode::File { content: LOGIN_ELF }),
    ("login.elf", RamdiskNode::File { content: LOGIN_ELF }),
    ("su", RamdiskNode::File { content: SU_ELF }),
    ("su.elf", RamdiskNode::File { content: SU_ELF }),
    (
        "passwd",
        RamdiskNode::File {
            content: PASSWD_ELF,
        },
    ),
    (
        "passwd.elf",
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
    (
        "adduser.elf",
        RamdiskNode::File {
            content: ADDUSER_ELF,
        },
    ),
    ("id", RamdiskNode::File { content: ID_ELF }),
    ("id.elf", RamdiskNode::File { content: ID_ELF }),
    (
        "whoami",
        RamdiskNode::File {
            content: WHOAMI_ELF,
        },
    ),
    (
        "whoami.elf",
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
    (
        "telnetd.elf",
        RamdiskNode::File {
            content: TELNETD_ELF,
        },
    ),
    // Phase 32: build tools and utilities
    ("touch", RamdiskNode::File { content: TOUCH_ELF }),
    ("touch.elf", RamdiskNode::File { content: TOUCH_ELF }),
    ("stat", RamdiskNode::File { content: STAT_ELF }),
    ("stat.elf", RamdiskNode::File { content: STAT_ELF }),
    ("wc", RamdiskNode::File { content: WC_ELF }),
    ("wc.elf", RamdiskNode::File { content: WC_ELF }),
    ("ar", RamdiskNode::File { content: AR_ELF }),
    ("ar.elf", RamdiskNode::File { content: AR_ELF }),
    (
        "install",
        RamdiskNode::File {
            content: INSTALL_ELF,
        },
    ),
    (
        "install.elf",
        RamdiskNode::File {
            content: INSTALL_ELF,
        },
    ),
    ("make", RamdiskNode::File { content: MAKE_ELF }),
    ("make.elf", RamdiskNode::File { content: MAKE_ELF }),
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

static SBIN_ENTRIES: &[(&str, RamdiskNode)] = &[
    ("init", RamdiskNode::File { content: INIT_ELF }),
    ("init.elf", RamdiskNode::File { content: INIT_ELF }),
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
/// Accepts both absolute (`/bin/cat.elf`) and relative (`bin/cat.elf`) paths;
/// leading slashes are stripped before traversal. An empty path returns root.
///
/// # Examples
///
/// ```ignore
/// ramdisk_lookup("/")              // → root Dir
/// ramdisk_lookup("/bin")           // → bin Dir
/// ramdisk_lookup("/bin/cat.elf")   // → File
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
/// bare filename such as `"cat.elf"` is searched under `/bin/` and then
/// `/etc/`.
///
/// Used by `sys_open`, `sys_execve`, and `resolve_command`.
pub fn get_file(name: &str) -> Option<&'static [u8]> {
    // Try exact path first — avoid allocation when already absolute.
    if name.starts_with('/') {
        if let Some(RamdiskNode::File { content }) = ramdisk_lookup(name) {
            return Some(content);
        }
        // Try with .elf suffix (ramdisk binaries use this extension).
        if !name.ends_with(".elf") {
            let elf_path = alloc::format!("{}.elf", name);
            if let Some(RamdiskNode::File { content }) = ramdisk_lookup(&elf_path) {
                return Some(content);
            }
        }
    } else {
        let path = alloc::format!("/{}", name);
        if let Some(RamdiskNode::File { content }) = ramdisk_lookup(&path) {
            return Some(content);
        }
        if !name.ends_with(".elf") {
            let elf_path = alloc::format!("/{}.elf", name);
            if let Some(RamdiskNode::File { content }) = ramdisk_lookup(&elf_path) {
                return Some(content);
            }
        }
    }

    // Backward compatibility: try under /bin/ and /etc/ for bare filenames.
    if !name.contains('/') {
        let bin_path = alloc::format!("/bin/{}", name);
        if let Some(RamdiskNode::File { content }) = ramdisk_lookup(&bin_path) {
            return Some(content);
        }
        if !name.ends_with(".elf") {
            let bin_elf = alloc::format!("/bin/{}.elf", name);
            if let Some(RamdiskNode::File { content }) = ramdisk_lookup(&bin_elf) {
                return Some(content);
            }
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
        name: "exit0.elf",
        content: EXIT0_ELF,
    },
    FlatFile {
        name: "fork-test.elf",
        content: FORK_TEST_ELF,
    },
    FlatFile {
        name: "echo-args.elf",
        content: ECHO_ARGS_ELF,
    },
    FlatFile {
        name: "hello.elf",
        content: HELLO_ELF,
    },
    FlatFile {
        name: "tmpfs-test.elf",
        content: TMPFS_TEST_ELF,
    },
    FlatFile {
        name: "echo.elf",
        content: ECHO_ELF,
    },
    FlatFile {
        name: "true.elf",
        content: TRUE_ELF,
    },
    FlatFile {
        name: "false.elf",
        content: FALSE_ELF,
    },
    FlatFile {
        name: "cat.elf",
        content: CAT_ELF,
    },
    FlatFile {
        name: "ls.elf",
        content: LS_ELF,
    },
    FlatFile {
        name: "pwd.elf",
        content: PWD_ELF,
    },
    FlatFile {
        name: "mkdir.elf",
        content: MKDIR_ELF,
    },
    FlatFile {
        name: "rmdir.elf",
        content: RMDIR_ELF,
    },
    FlatFile {
        name: "rm.elf",
        content: RM_ELF,
    },
    FlatFile {
        name: "cp.elf",
        content: CP_ELF,
    },
    FlatFile {
        name: "mv.elf",
        content: MV_ELF,
    },
    FlatFile {
        name: "env.elf",
        content: ENV_ELF,
    },
    FlatFile {
        name: "sleep.elf",
        content: SLEEP_ELF,
    },
    FlatFile {
        name: "grep.elf",
        content: GREP_ELF,
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
