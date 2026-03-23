//! Ramdisk filesystem backend — Phase 8 (`fat_server` handler logic).
//!
//! Embeds a fixed set of files at compile time and exposes a single
//! [`handle`] function that processes one IPC message and returns the reply.
//! No heap allocation, no mutable state — the ramdisk is purely read-only.
//!
//! # Phase 8 limitations
//!
//! File descriptors are simple indices into [`FILES`].  Because all clients
//! are kernel tasks sharing the same address space, `FILE_OPEN` receives a
//! raw kernel pointer to the name string and `FILE_READ` returns a raw pointer
//! into the static content slice.  Both shortcuts are removed in Phase 9+
//! when ring-3 clients require proper page-capability grants.

#![allow(dead_code)]

use crate::fs::protocol::{
    FILE_CLOSE, FILE_LIST, FILE_OPEN, FILE_READ, MAX_LIST_LEN, MAX_NAME_LEN, MAX_READ_LEN,
};
use crate::ipc::Message;

// ---------------------------------------------------------------------------
// Static file table
// ---------------------------------------------------------------------------

struct RamdiskFile {
    name: &'static str,
    content: &'static [u8],
}

const FILES: &[RamdiskFile] = &[
    RamdiskFile {
        name: "hello.txt",
        content: include_bytes!("../../initrd/hello.txt"),
    },
    RamdiskFile {
        name: "readme.txt",
        content: include_bytes!("../../initrd/readme.txt"),
    },
];

// ---------------------------------------------------------------------------
// Static name list (null-separated, for FILE_LIST)
// ---------------------------------------------------------------------------

const fn file_name_list_len() -> usize {
    let mut total = 0;
    let mut index = 0;
    while index < FILES.len() {
        total += FILES[index].name.len() + 1;
        index += 1;
    }
    total
}

const FILE_NAME_LIST_LEN: usize = file_name_list_len();

const fn build_file_name_list() -> [u8; FILE_NAME_LIST_LEN] {
    let mut buf = [0; FILE_NAME_LIST_LEN];
    let mut out = 0;
    let mut file_index = 0;
    while file_index < FILES.len() {
        let name = FILES[file_index].name.as_bytes();
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
    debug_assert!(
        FILE_NAME_LIST.len() <= MAX_LIST_LEN,
        "FILE_LIST buffer exceeds protocol limit"
    );
    (FILE_NAME_LIST.as_ptr(), FILE_NAME_LIST.len())
}

// ---------------------------------------------------------------------------
// Message handler
// ---------------------------------------------------------------------------

/// Handle one `fat_server` IPC message and return the reply [`Message`].
///
/// Dispatches on `msg.label`:
/// - [`FILE_OPEN`]  — look up a file by name; reply with its fd or `u64::MAX`.
/// - [`FILE_READ`]  — return a pointer + length into the static content.
/// - [`FILE_CLOSE`] — no-op; reply with an empty ack message.
/// - anything else  — reply with label `u64::MAX` (unknown operation).
pub fn handle(msg: &Message) -> Message {
    match msg.label {
        FILE_OPEN => handle_open(msg),
        FILE_READ => handle_read(msg),
        FILE_CLOSE => Message::new(0),
        FILE_LIST => {
            // Return a pointer to the static null-separated name list.
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
// FILE_OPEN
// ---------------------------------------------------------------------------

fn handle_open(msg: &Message) -> Message {
    let ptr = msg.data[0];
    let len = msg.data[1] as usize;

    // Null-ptr / zero-length / oversized-name guard.
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

    for (index, file) in FILES.iter().enumerate() {
        if file.name == name {
            return Message::with1(0, index as u64);
        }
    }

    Message::with1(0, u64::MAX)
}

// ---------------------------------------------------------------------------
// FILE_READ
// ---------------------------------------------------------------------------

fn handle_read(msg: &Message) -> Message {
    let fd = msg.data[0];
    let offset = msg.data[1] as usize;
    let max_len = msg.data[2] as usize;

    // Reject fd values that exceed usize range or the file table bounds.
    let fd_usize = match usize::try_from(fd) {
        Ok(v) => v,
        Err(_) => return Message::with2(0, 0, 0),
    };
    if fd_usize >= FILES.len() {
        return Message::with2(0, 0, 0);
    }

    let file = &FILES[fd_usize];

    // Reject offsets past the end of the file.
    if offset > file.content.len() {
        return Message::with2(0, 0, 0);
    }

    let available = file.content.len() - offset;
    let actual_len = available.min(max_len).min(MAX_READ_LEN);

    // Return a pointer into the static content slice.  The slice lives for
    // 'static so the pointer remains valid for as long as the kernel runs.
    let content_ptr = file.content[offset..].as_ptr() as u64;

    Message::with2(0, content_ptr, actual_len as u64)
}
