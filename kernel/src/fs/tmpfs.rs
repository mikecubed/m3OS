//! In-memory filesystem (tmpfs) — Phase 13.
//!
//! Provides a RAM-backed writable filesystem mounted at `/tmp`.  File data
//! is stored in `Vec<u8>` buffers allocated from the kernel heap; directory
//! structure is a tree of [`TmpfsNode`] entries.
//!
//! All operations go through the global [`TMPFS`] instance, which is
//! protected by a [`spin::Mutex`].  This is fine for a single-CPU kernel.
//!
//! # Limitations (Phase 13)
//!
//! - Data is lost on reboot (RAM-backed, no persistence).
//! - No file permissions or ownership.
//! - No hard/symbolic links.
//! - No file-backed mmap.

#![allow(dead_code)]

extern crate alloc;

use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    vec::Vec,
};
use spin::Mutex;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Maximum size of a single tmpfs file (16 MiB).
///
/// Prevents userspace from exhausting the kernel heap via unbounded
/// write/truncate calls.
pub const MAX_FILE_SIZE: usize = 16 * 1024 * 1024;

/// Errors returned by tmpfs operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmpfsError {
    /// The path does not exist.
    NotFound,
    /// The target already exists.
    AlreadyExists,
    /// Expected a file but found a directory (or vice versa).
    WrongType,
    /// Tried to remove a non-empty directory.
    NotEmpty,
    /// An intermediate path component is not a directory.
    NotADirectory,
    /// The path is invalid (empty or malformed).
    InvalidPath,
    /// File would exceed the maximum size limit.
    NoSpace,
}

// ---------------------------------------------------------------------------
// Node types
// ---------------------------------------------------------------------------

/// Metadata returned by `stat`.
#[derive(Debug, Clone, Copy)]
pub struct TmpfsStat {
    /// True if this node is a directory, false if a file.
    pub is_dir: bool,
    /// File size in bytes (0 for directories).
    pub size: usize,
}

/// A single node in the tmpfs tree.
enum TmpfsNode {
    File(FileData),
    Dir(DirData),
}

struct FileData {
    content: Vec<u8>,
}

struct DirData {
    children: BTreeMap<String, TmpfsNode>,
}

impl DirData {
    fn new() -> Self {
        DirData {
            children: BTreeMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tmpfs instance
// ---------------------------------------------------------------------------

/// A complete tmpfs filesystem instance.
pub struct Tmpfs {
    root: TmpfsNode,
}

impl Tmpfs {
    /// Create a new empty tmpfs with an empty root directory.
    ///
    /// `BTreeMap::new()` is const-evaluable since Rust 1.66, so this
    /// constructor can initialize the global `TMPFS` at compile time.
    const fn new() -> Self {
        Tmpfs {
            root: TmpfsNode::Dir(DirData {
                children: BTreeMap::new(),
            }),
        }
    }

    // -- Path helpers -------------------------------------------------------

    /// Split a path like "foo/bar/baz" into components, stripping leading `/`.
    fn components(path: &str) -> impl Iterator<Item = &str> {
        path.trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
    }

    /// Navigate to the parent directory of `path` and return it along with
    /// the final component name.  Returns `None` if path is empty or root.
    fn parent_and_name<'a, 'b>(
        &'a mut self,
        path: &'b str,
    ) -> Result<(&'a mut DirData, &'b str), TmpfsError> {
        let trimmed = path.trim_start_matches('/');
        if trimmed.is_empty() {
            return Err(TmpfsError::InvalidPath);
        }

        let parts: Vec<&str> = Self::components(path).collect();
        if parts.is_empty() {
            return Err(TmpfsError::InvalidPath);
        }

        let (parent_parts, name) = parts.split_at(parts.len() - 1);
        let name = name[0];

        // Walk to the parent directory.
        let mut current = &mut self.root;
        for part in parent_parts {
            current = match current {
                TmpfsNode::Dir(dir) => match dir.children.get_mut(*part) {
                    Some(child) => child,
                    None => return Err(TmpfsError::NotFound),
                },
                TmpfsNode::File(_) => return Err(TmpfsError::NotADirectory),
            };
        }

        match current {
            TmpfsNode::Dir(dir) => Ok((dir, name)),
            TmpfsNode::File(_) => Err(TmpfsError::NotADirectory),
        }
    }

    /// Navigate to a node by path.
    fn get_node(&self, path: &str) -> Result<&TmpfsNode, TmpfsError> {
        let mut current = &self.root;
        for part in Self::components(path) {
            current = match current {
                TmpfsNode::Dir(dir) => match dir.children.get(part) {
                    Some(child) => child,
                    None => return Err(TmpfsError::NotFound),
                },
                TmpfsNode::File(_) => return Err(TmpfsError::NotADirectory),
            };
        }
        Ok(current)
    }

    /// Navigate to a mutable node by path.
    fn get_node_mut(&mut self, path: &str) -> Result<&mut TmpfsNode, TmpfsError> {
        let mut current = &mut self.root;
        for part in Self::components(path) {
            current = match current {
                TmpfsNode::Dir(dir) => match dir.children.get_mut(part) {
                    Some(child) => child,
                    None => return Err(TmpfsError::NotFound),
                },
                TmpfsNode::File(_) => return Err(TmpfsError::NotADirectory),
            };
        }
        Ok(current)
    }

    // -- Public API ---------------------------------------------------------

    /// Create a new empty file at `path`.  Parent directories must exist.
    pub fn create_file(&mut self, path: &str) -> Result<(), TmpfsError> {
        let (parent, name) = self.parent_and_name(path)?;
        if parent.children.contains_key(name) {
            return Err(TmpfsError::AlreadyExists);
        }
        parent.children.insert(
            name.to_string(),
            TmpfsNode::File(FileData {
                content: Vec::new(),
            }),
        );
        Ok(())
    }

    /// Write `data` to the file at `path` starting at byte `offset`.
    ///
    /// Extends the file if `offset + data.len()` exceeds the current size.
    /// The file must already exist.
    pub fn write_file(&mut self, path: &str, offset: usize, data: &[u8]) -> Result<(), TmpfsError> {
        let node = self.get_node_mut(path)?;
        match node {
            TmpfsNode::File(file) => {
                let end = offset.checked_add(data.len()).ok_or(TmpfsError::NoSpace)?;
                if end > MAX_FILE_SIZE {
                    return Err(TmpfsError::NoSpace);
                }
                if end > file.content.len() {
                    file.content.resize(end, 0);
                }
                file.content[offset..end].copy_from_slice(data);
                Ok(())
            }
            TmpfsNode::Dir(_) => Err(TmpfsError::WrongType),
        }
    }

    /// Read up to `max_len` bytes from the file at `path` starting at `offset`.
    ///
    /// Returns the bytes read.  Returns an empty slice if `offset` is at or
    /// past the end of the file.
    pub fn read_file(
        &self,
        path: &str,
        offset: usize,
        max_len: usize,
    ) -> Result<&[u8], TmpfsError> {
        let node = self.get_node(path)?;
        match node {
            TmpfsNode::File(file) => {
                if offset >= file.content.len() {
                    return Ok(&[]);
                }
                // Use saturating_add to prevent overflow from userspace-controlled max_len.
                let end = offset.saturating_add(max_len).min(file.content.len());
                Ok(&file.content[offset..end])
            }
            TmpfsNode::Dir(_) => Err(TmpfsError::WrongType),
        }
    }

    /// Return metadata for the node at `path`.
    pub fn stat(&self, path: &str) -> Result<TmpfsStat, TmpfsError> {
        let node = self.get_node(path)?;
        match node {
            TmpfsNode::File(file) => Ok(TmpfsStat {
                is_dir: false,
                size: file.content.len(),
            }),
            TmpfsNode::Dir(_) => Ok(TmpfsStat {
                is_dir: true,
                size: 0,
            }),
        }
    }

    /// Delete a file at `path`.  Fails if `path` is a directory.
    pub fn unlink(&mut self, path: &str) -> Result<(), TmpfsError> {
        let (parent, name) = self.parent_and_name(path)?;
        match parent.children.get(name) {
            Some(TmpfsNode::File(_)) => {
                parent.children.remove(name);
                Ok(())
            }
            Some(TmpfsNode::Dir(_)) => Err(TmpfsError::WrongType),
            None => Err(TmpfsError::NotFound),
        }
    }

    /// Create a new directory at `path`.  Parent directories must exist.
    pub fn mkdir(&mut self, path: &str) -> Result<(), TmpfsError> {
        let (parent, name) = self.parent_and_name(path)?;
        if parent.children.contains_key(name) {
            return Err(TmpfsError::AlreadyExists);
        }
        parent
            .children
            .insert(name.to_string(), TmpfsNode::Dir(DirData::new()));
        Ok(())
    }

    /// Remove an empty directory at `path`.
    pub fn rmdir(&mut self, path: &str) -> Result<(), TmpfsError> {
        let (parent, name) = self.parent_and_name(path)?;
        match parent.children.get(name) {
            Some(TmpfsNode::Dir(dir)) => {
                if !dir.children.is_empty() {
                    return Err(TmpfsError::NotEmpty);
                }
                parent.children.remove(name);
                Ok(())
            }
            Some(TmpfsNode::File(_)) => Err(TmpfsError::WrongType),
            None => Err(TmpfsError::NotFound),
        }
    }

    /// List entries in the directory at `path`.
    ///
    /// Returns a vector of `(name, is_dir)` pairs.
    pub fn list_dir(&self, path: &str) -> Result<Vec<(String, bool)>, TmpfsError> {
        let node = self.get_node(path)?;
        match node {
            TmpfsNode::Dir(dir) => {
                let entries = dir
                    .children
                    .iter()
                    .map(|(name, node)| {
                        let is_dir = matches!(node, TmpfsNode::Dir(_));
                        (name.clone(), is_dir)
                    })
                    .collect();
                Ok(entries)
            }
            TmpfsNode::File(_) => Err(TmpfsError::WrongType),
        }
    }

    /// Rename/move a file or directory from `old_path` to `new_path`.
    ///
    /// Cross-directory moves are supported as long as both parent paths
    /// resolve to existing directories.  Moving a directory into its own
    /// subtree (e.g. "a" → "a/b") is rejected with `InvalidPath`.
    pub fn rename(&mut self, old_path: &str, new_path: &str) -> Result<(), TmpfsError> {
        let old_normalized = old_path.trim_start_matches('/').trim_end_matches('/');
        let new_normalized = new_path.trim_start_matches('/').trim_end_matches('/');

        if old_normalized.is_empty() || new_normalized.is_empty() {
            return Err(TmpfsError::InvalidPath);
        }

        // Reject moving a directory into its own subtree.
        if new_normalized.starts_with(old_normalized)
            && new_normalized.as_bytes().get(old_normalized.len()) == Some(&b'/')
        {
            return Err(TmpfsError::InvalidPath);
        }

        // Remove source node.
        let (parent, old_name) = self.parent_and_name(old_path)?;
        let node = parent
            .children
            .remove(old_name)
            .ok_or(TmpfsError::NotFound)?;

        // Try to insert at destination — rollback on failure.
        match self.parent_and_name(new_path) {
            Ok((new_parent, new_name)) => {
                if let Some(existing) = new_parent.children.get(new_name) {
                    // POSIX rename semantics: validate type compatibility.
                    let reject = match (&node, existing) {
                        // File ↔ directory replacement is not allowed.
                        (TmpfsNode::File(_), TmpfsNode::Dir(_))
                        | (TmpfsNode::Dir(_), TmpfsNode::File(_)) => Some(TmpfsError::WrongType),
                        // Replacing a non-empty directory is not allowed.
                        (_, TmpfsNode::Dir(dst)) if !dst.children.is_empty() => {
                            Some(TmpfsError::NotEmpty)
                        }
                        // Same-type (file↔file, dir↔empty-dir) is OK.
                        _ => None,
                    };
                    if let Some(err) = reject {
                        // Rollback source.
                        let (old_parent, old_name) = self
                            .parent_and_name(old_path)
                            .expect("rollback: source parent must still exist");
                        old_parent.children.insert(old_name.to_string(), node);
                        return Err(err);
                    }
                    new_parent.children.remove(new_name);
                }
                new_parent.children.insert(new_name.to_string(), node);
                Ok(())
            }
            Err(e) => {
                // Rollback: re-insert source node at its original location.
                let (old_parent, old_name) = self
                    .parent_and_name(old_path)
                    .expect("rollback: source parent must still exist");
                old_parent.children.insert(old_name.to_string(), node);
                Err(e)
            }
        }
    }

    /// Truncate a file to `new_size` bytes.
    ///
    /// If `new_size` is larger than the current size, the file is extended
    /// with zero bytes.
    pub fn truncate(&mut self, path: &str, new_size: usize) -> Result<(), TmpfsError> {
        if new_size > MAX_FILE_SIZE {
            return Err(TmpfsError::NoSpace);
        }
        let node = self.get_node_mut(path)?;
        match node {
            TmpfsNode::File(file) => {
                file.content.resize(new_size, 0);
                Ok(())
            }
            TmpfsNode::Dir(_) => Err(TmpfsError::WrongType),
        }
    }

    /// Open a file at `path`, creating it if `create` is true.
    ///
    /// Returns `Ok(true)` if a new file was created, `Ok(false)` if it
    /// already existed.  Returns an error if the path is a directory.
    pub fn open_or_create(&mut self, path: &str, create: bool) -> Result<bool, TmpfsError> {
        match self.get_node(path) {
            Ok(TmpfsNode::File(_)) => Ok(false),
            Ok(TmpfsNode::Dir(_)) => Err(TmpfsError::WrongType),
            Err(TmpfsError::NotFound) if create => {
                self.create_file(path)?;
                Ok(true)
            }
            Err(e) => Err(e),
        }
    }

    /// Get the size of a file.
    pub fn file_size(&self, path: &str) -> Result<usize, TmpfsError> {
        let node = self.get_node(path)?;
        match node {
            TmpfsNode::File(file) => Ok(file.content.len()),
            TmpfsNode::Dir(_) => Err(TmpfsError::WrongType),
        }
    }
}

// ---------------------------------------------------------------------------
// Global instance
// ---------------------------------------------------------------------------

/// Global tmpfs instance mounted at `/tmp`.
pub static TMPFS: Mutex<Tmpfs> = Mutex::new(Tmpfs::new());
