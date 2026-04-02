extern crate alloc;

use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    vec::Vec,
};

/// Maximum size of a single tmpfs file (16 MiB).
pub const MAX_FILE_SIZE: usize = 16 * 1024 * 1024;

/// Errors returned by tmpfs operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmpfsError {
    NotFound,
    AlreadyExists,
    WrongType,
    NotEmpty,
    NotADirectory,
    InvalidPath,
    NoSpace,
}

/// Metadata returned by `stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TmpfsStat {
    pub is_dir: bool,
    pub is_symlink: bool,
    pub ino: u64,
    pub nlink: u64,
    pub size: usize,
    /// Owner user ID (Phase 27).
    pub uid: u32,
    /// Owner group ID (Phase 27).
    pub gid: u32,
    /// Unix permission mode bits (Phase 27). E.g. 0o755 for dirs, 0o644 for files.
    pub mode: u16,
}

enum TmpfsNode {
    File(FileData),
    Dir(DirData),
    Symlink(SymlinkData),
}

struct FileData {
    inode: u64,
    content: Vec<u8>,
    uid: u32,
    gid: u32,
    mode: u16,
}

struct DirData {
    inode: u64,
    children: BTreeMap<String, TmpfsNode>,
    uid: u32,
    gid: u32,
    mode: u16,
}

struct SymlinkData {
    inode: u64,
    target: String,
    uid: u32,
    gid: u32,
}

/// A complete tmpfs filesystem instance.
pub struct Tmpfs {
    root: TmpfsNode,
    next_inode: u64,
}

impl Tmpfs {
    pub const fn new() -> Self {
        Tmpfs {
            root: TmpfsNode::Dir(DirData {
                inode: 1,
                children: BTreeMap::new(),
                uid: 0,
                gid: 0,
                mode: 0o755,
            }),
            next_inode: 2,
        }
    }

    fn alloc_inode(&mut self) -> u64 {
        let inode = self.next_inode;
        self.next_inode = self.next_inode.saturating_add(1);
        inode
    }

    fn components(path: &str) -> impl Iterator<Item = &str> {
        path.trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
    }

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

        let mut current = &mut self.root;
        for part in parent_parts {
            current = match current {
                TmpfsNode::Dir(dir) => match dir.children.get_mut(*part) {
                    Some(child) => child,
                    None => return Err(TmpfsError::NotFound),
                },
                TmpfsNode::File(_) | TmpfsNode::Symlink(_) => {
                    return Err(TmpfsError::NotADirectory);
                }
            };
        }

        match current {
            TmpfsNode::Dir(dir) => Ok((dir, name)),
            TmpfsNode::File(_) | TmpfsNode::Symlink(_) => Err(TmpfsError::NotADirectory),
        }
    }

    fn get_node(&self, path: &str) -> Result<&TmpfsNode, TmpfsError> {
        let mut current = &self.root;
        for part in Self::components(path) {
            current = match current {
                TmpfsNode::Dir(dir) => match dir.children.get(part) {
                    Some(child) => child,
                    None => return Err(TmpfsError::NotFound),
                },
                TmpfsNode::File(_) | TmpfsNode::Symlink(_) => {
                    return Err(TmpfsError::NotADirectory);
                }
            };
        }
        Ok(current)
    }

    fn get_node_mut(&mut self, path: &str) -> Result<&mut TmpfsNode, TmpfsError> {
        let mut current = &mut self.root;
        for part in Self::components(path) {
            current = match current {
                TmpfsNode::Dir(dir) => match dir.children.get_mut(part) {
                    Some(child) => child,
                    None => return Err(TmpfsError::NotFound),
                },
                TmpfsNode::File(_) | TmpfsNode::Symlink(_) => {
                    return Err(TmpfsError::NotADirectory);
                }
            };
        }
        Ok(current)
    }

    pub fn create_file(&mut self, path: &str) -> Result<(), TmpfsError> {
        self.create_file_with_meta(path, 0, 0, 0o644)
    }

    /// Create a file with specific ownership and mode.
    pub fn create_file_with_meta(
        &mut self,
        path: &str,
        uid: u32,
        gid: u32,
        mode: u16,
    ) -> Result<(), TmpfsError> {
        let inode = self.alloc_inode();
        let (parent, name) = self.parent_and_name(path)?;
        if parent.children.contains_key(name) {
            return Err(TmpfsError::AlreadyExists);
        }
        parent.children.insert(
            name.to_string(),
            TmpfsNode::File(FileData {
                inode,
                content: Vec::new(),
                uid,
                gid,
                mode,
            }),
        );
        Ok(())
    }

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
            TmpfsNode::Dir(_) | TmpfsNode::Symlink(_) => Err(TmpfsError::WrongType),
        }
    }

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
                let end = offset.saturating_add(max_len).min(file.content.len());
                Ok(&file.content[offset..end])
            }
            TmpfsNode::Dir(_) | TmpfsNode::Symlink(_) => Err(TmpfsError::WrongType),
        }
    }

    pub fn stat(&self, path: &str) -> Result<TmpfsStat, TmpfsError> {
        let node = self.get_node(path)?;
        match node {
            TmpfsNode::File(file) => Ok(TmpfsStat {
                is_dir: false,
                is_symlink: false,
                ino: file.inode,
                nlink: 1,
                size: file.content.len(),
                uid: file.uid,
                gid: file.gid,
                mode: file.mode,
            }),
            TmpfsNode::Dir(dir) => Ok(TmpfsStat {
                is_dir: true,
                is_symlink: false,
                ino: dir.inode,
                nlink: 2 + dir
                    .children
                    .values()
                    .filter(|child| matches!(child, TmpfsNode::Dir(_)))
                    .count() as u64,
                size: 0,
                uid: dir.uid,
                gid: dir.gid,
                mode: dir.mode,
            }),
            TmpfsNode::Symlink(link) => Ok(TmpfsStat {
                is_dir: false,
                is_symlink: true,
                ino: link.inode,
                nlink: 1,
                size: link.target.len(),
                uid: link.uid,
                gid: link.gid,
                mode: 0o777,
            }),
        }
    }

    pub fn unlink(&mut self, path: &str) -> Result<(), TmpfsError> {
        let (parent, name) = self.parent_and_name(path)?;
        match parent.children.get(name) {
            Some(TmpfsNode::File(_) | TmpfsNode::Symlink(_)) => {
                parent.children.remove(name);
                Ok(())
            }
            Some(TmpfsNode::Dir(_)) => Err(TmpfsError::WrongType),
            None => Err(TmpfsError::NotFound),
        }
    }

    pub fn mkdir(&mut self, path: &str) -> Result<(), TmpfsError> {
        self.mkdir_with_meta(path, 0, 0, 0o755)
    }

    /// Create a directory with specific ownership and mode.
    pub fn mkdir_with_meta(
        &mut self,
        path: &str,
        uid: u32,
        gid: u32,
        mode: u16,
    ) -> Result<(), TmpfsError> {
        let inode = self.alloc_inode();
        let (parent, name) = self.parent_and_name(path)?;
        if parent.children.contains_key(name) {
            return Err(TmpfsError::AlreadyExists);
        }
        parent.children.insert(
            name.to_string(),
            TmpfsNode::Dir(DirData {
                inode,
                children: BTreeMap::new(),
                uid,
                gid,
                mode,
            }),
        );
        Ok(())
    }

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
            Some(TmpfsNode::File(_) | TmpfsNode::Symlink(_)) => Err(TmpfsError::WrongType),
            None => Err(TmpfsError::NotFound),
        }
    }

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
            TmpfsNode::File(_) | TmpfsNode::Symlink(_) => Err(TmpfsError::WrongType),
        }
    }

    pub fn rename(&mut self, old_path: &str, new_path: &str) -> Result<(), TmpfsError> {
        let old_normalized = old_path.trim_start_matches('/').trim_end_matches('/');
        let new_normalized = new_path.trim_start_matches('/').trim_end_matches('/');

        if old_normalized.is_empty() || new_normalized.is_empty() {
            return Err(TmpfsError::InvalidPath);
        }

        if new_normalized.starts_with(old_normalized)
            && new_normalized.as_bytes().get(old_normalized.len()) == Some(&b'/')
        {
            return Err(TmpfsError::InvalidPath);
        }

        let (parent, old_name) = self.parent_and_name(old_path)?;
        let node = parent
            .children
            .remove(old_name)
            .ok_or(TmpfsError::NotFound)?;

        match self.parent_and_name(new_path) {
            Ok((new_parent, new_name)) => {
                if let Some(existing) = new_parent.children.get(new_name) {
                    let reject = match (&node, existing) {
                        (TmpfsNode::File(_) | TmpfsNode::Symlink(_), TmpfsNode::Dir(_))
                        | (TmpfsNode::Dir(_), TmpfsNode::File(_) | TmpfsNode::Symlink(_)) => {
                            Some(TmpfsError::WrongType)
                        }
                        (_, TmpfsNode::Dir(dst)) if !dst.children.is_empty() => {
                            Some(TmpfsError::NotEmpty)
                        }
                        _ => None,
                    };
                    if let Some(err) = reject {
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
                let (old_parent, old_name) = self
                    .parent_and_name(old_path)
                    .expect("rollback: source parent must still exist");
                old_parent.children.insert(old_name.to_string(), node);
                Err(e)
            }
        }
    }

    /// Change the permission mode of a file or directory.
    /// Symlinks are silently ignored (POSIX: lchmod is a no-op on most systems).
    pub fn chmod(&mut self, path: &str, mode: u16) -> Result<(), TmpfsError> {
        let node = self.get_node_mut(path)?;
        match node {
            TmpfsNode::File(f) => f.mode = mode,
            TmpfsNode::Dir(d) => d.mode = mode,
            TmpfsNode::Symlink(_) => {}
        }
        Ok(())
    }

    /// Change the owner uid/gid of a file, directory, or symlink.
    pub fn chown(&mut self, path: &str, uid: u32, gid: u32) -> Result<(), TmpfsError> {
        let node = self.get_node_mut(path)?;
        match node {
            TmpfsNode::File(f) => {
                f.uid = uid;
                f.gid = gid;
            }
            TmpfsNode::Dir(d) => {
                d.uid = uid;
                d.gid = gid;
            }
            TmpfsNode::Symlink(s) => {
                s.uid = uid;
                s.gid = gid;
            }
        }
        Ok(())
    }

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
            TmpfsNode::Dir(_) | TmpfsNode::Symlink(_) => Err(TmpfsError::WrongType),
        }
    }

    pub fn open_or_create(&mut self, path: &str, create: bool) -> Result<bool, TmpfsError> {
        self.open_or_create_with_meta(path, create, 0, 0, 0o644)
    }

    /// Open an existing file or create it with specific ownership and mode.
    pub fn open_or_create_with_meta(
        &mut self,
        path: &str,
        create: bool,
        uid: u32,
        gid: u32,
        mode: u16,
    ) -> Result<bool, TmpfsError> {
        match self.get_node(path) {
            Ok(TmpfsNode::File(_)) => Ok(false),
            Ok(TmpfsNode::Dir(_) | TmpfsNode::Symlink(_)) => Err(TmpfsError::WrongType),
            Err(TmpfsError::NotFound) if create => {
                self.create_file_with_meta(path, uid, gid, mode)?;
                Ok(true)
            }
            Err(e) => Err(e),
        }
    }

    pub fn file_size(&self, path: &str) -> Result<usize, TmpfsError> {
        let node = self.get_node(path)?;
        match node {
            TmpfsNode::File(file) => Ok(file.content.len()),
            TmpfsNode::Dir(_) | TmpfsNode::Symlink(_) => Err(TmpfsError::WrongType),
        }
    }

    /// Create a symbolic link at `path` pointing to `target`.
    pub fn create_symlink(&mut self, path: &str, target: &str) -> Result<(), TmpfsError> {
        self.create_symlink_with_meta(path, target, 0, 0)
    }

    /// Create a symbolic link with specific ownership.
    pub fn create_symlink_with_meta(
        &mut self,
        path: &str,
        target: &str,
        uid: u32,
        gid: u32,
    ) -> Result<(), TmpfsError> {
        let inode = self.alloc_inode();
        let (parent, name) = self.parent_and_name(path)?;
        if parent.children.contains_key(name) {
            return Err(TmpfsError::AlreadyExists);
        }
        parent.children.insert(
            name.to_string(),
            TmpfsNode::Symlink(SymlinkData {
                inode,
                target: target.to_string(),
                uid,
                gid,
            }),
        );
        Ok(())
    }

    /// Read the target of a symbolic link at `path`.
    pub fn read_symlink(&self, path: &str) -> Result<&str, TmpfsError> {
        let node = self.get_node(path)?;
        match node {
            TmpfsNode::Symlink(link) => Ok(&link.target),
            TmpfsNode::File(_) | TmpfsNode::Dir(_) => Err(TmpfsError::WrongType),
        }
    }
}

impl Default for Tmpfs {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_read_file() {
        let mut fs = Tmpfs::new();
        fs.create_file("/hello.txt").unwrap();
        fs.write_file("/hello.txt", 0, b"world").unwrap();
        let data = fs.read_file("/hello.txt", 0, 100).unwrap();
        assert_eq!(data, b"world");
    }

    #[test]
    fn write_at_offset() {
        let mut fs = Tmpfs::new();
        fs.create_file("/f").unwrap();
        fs.write_file("/f", 0, b"AAAA").unwrap();
        fs.write_file("/f", 2, b"BB").unwrap();
        let data = fs.read_file("/f", 0, 100).unwrap();
        assert_eq!(data, b"AABB");
    }

    #[test]
    fn stat_file_and_dir() {
        let mut fs = Tmpfs::new();
        fs.create_file("/f").unwrap();
        fs.write_file("/f", 0, b"abc").unwrap();
        fs.mkdir("/d").unwrap();

        let fstat = fs.stat("/f").unwrap();
        assert!(!fstat.is_dir);
        assert!(!fstat.is_symlink);
        assert_ne!(fstat.ino, 0);
        assert_eq!(fstat.nlink, 1);
        assert_eq!(fstat.size, 3);

        let dstat = fs.stat("/d").unwrap();
        assert!(dstat.is_dir);
        assert!(!dstat.is_symlink);
        assert_ne!(dstat.ino, 0);
        assert_eq!(dstat.nlink, 2);
        assert_eq!(dstat.size, 0);

        // Root is a directory
        let rstat = fs.stat("/").unwrap();
        assert!(rstat.is_dir);
        assert!(!rstat.is_symlink);
        assert_eq!(rstat.ino, 1);
        assert_eq!(rstat.nlink, 3);
    }

    #[test]
    fn unlink_file() {
        let mut fs = Tmpfs::new();
        fs.create_file("/f").unwrap();
        fs.unlink("/f").unwrap();
        assert_eq!(fs.stat("/f"), Err(TmpfsError::NotFound));
    }

    #[test]
    fn mkdir_and_rmdir() {
        let mut fs = Tmpfs::new();
        fs.mkdir("/dir").unwrap();
        assert!(fs.stat("/dir").unwrap().is_dir);
        fs.rmdir("/dir").unwrap();
        assert_eq!(fs.stat("/dir"), Err(TmpfsError::NotFound));
    }

    #[test]
    fn rmdir_not_empty() {
        let mut fs = Tmpfs::new();
        fs.mkdir("/dir").unwrap();
        fs.create_file("/dir/f").unwrap();
        assert_eq!(fs.rmdir("/dir"), Err(TmpfsError::NotEmpty));
    }

    #[test]
    fn list_dir() {
        let mut fs = Tmpfs::new();
        fs.create_file("/a").unwrap();
        fs.mkdir("/b").unwrap();

        let entries = fs.list_dir("/").unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.contains(&("a".to_string(), false)));
        assert!(entries.contains(&("b".to_string(), true)));
    }

    #[test]
    fn rename_file() {
        let mut fs = Tmpfs::new();
        fs.create_file("/old").unwrap();
        fs.write_file("/old", 0, b"data").unwrap();
        fs.rename("/old", "/new").unwrap();
        assert_eq!(fs.stat("/old"), Err(TmpfsError::NotFound));
        assert_eq!(fs.read_file("/new", 0, 100).unwrap(), b"data");
    }

    #[test]
    fn truncate_extend_and_shrink() {
        let mut fs = Tmpfs::new();
        fs.create_file("/f").unwrap();
        fs.write_file("/f", 0, b"hello").unwrap();

        fs.truncate("/f", 10).unwrap();
        assert_eq!(fs.file_size("/f").unwrap(), 10);

        fs.truncate("/f", 3).unwrap();
        let data = fs.read_file("/f", 0, 100).unwrap();
        assert_eq!(data, b"hel");
    }

    #[test]
    fn nested_paths() {
        let mut fs = Tmpfs::new();
        fs.mkdir("/a").unwrap();
        fs.mkdir("/a/b").unwrap();
        fs.create_file("/a/b/c.txt").unwrap();
        fs.write_file("/a/b/c.txt", 0, b"nested").unwrap();
        assert_eq!(fs.read_file("/a/b/c.txt", 0, 100).unwrap(), b"nested");
    }

    #[test]
    fn error_cases() {
        let mut fs = Tmpfs::new();

        // Double create
        fs.create_file("/f").unwrap();
        assert_eq!(fs.create_file("/f"), Err(TmpfsError::AlreadyExists));

        // Unlink nonexistent
        assert_eq!(fs.unlink("/nope"), Err(TmpfsError::NotFound));

        // Write to directory
        fs.mkdir("/d").unwrap();
        assert_eq!(fs.write_file("/d", 0, b"x"), Err(TmpfsError::WrongType));

        // Read from directory
        assert_eq!(fs.read_file("/d", 0, 100), Err(TmpfsError::WrongType));

        // mkdir through a file
        assert_eq!(fs.mkdir("/f/sub"), Err(TmpfsError::NotADirectory));
    }

    #[test]
    fn open_or_create_file() {
        let mut fs = Tmpfs::new();
        assert_eq!(fs.open_or_create("/f", true), Ok(true));
        assert_eq!(fs.open_or_create("/f", true), Ok(false));
        assert_eq!(
            fs.open_or_create("/missing", false),
            Err(TmpfsError::NotFound)
        );
    }

    #[test]
    fn symlink_round_trip() {
        let mut fs = Tmpfs::new();
        fs.create_symlink("/link", "/some/target").unwrap();

        // read_symlink returns the target
        assert_eq!(fs.read_symlink("/link").unwrap(), "/some/target");

        // stat reports is_symlink and target length as size
        let st = fs.stat("/link").unwrap();
        assert!(st.is_symlink);
        assert!(!st.is_dir);
        assert_ne!(st.ino, 0);
        assert_eq!(st.nlink, 1);
        assert_eq!(st.size, "/some/target".len());
    }

    #[test]
    fn readlink_on_non_symlink_returns_error() {
        let mut fs = Tmpfs::new();
        fs.create_file("/f").unwrap();
        fs.mkdir("/d").unwrap();

        assert_eq!(fs.read_symlink("/f"), Err(TmpfsError::WrongType));
        assert_eq!(fs.read_symlink("/d"), Err(TmpfsError::WrongType));
    }

    #[test]
    fn unlink_symlink() {
        let mut fs = Tmpfs::new();
        fs.create_symlink("/link", "/target").unwrap();
        fs.unlink("/link").unwrap();
        assert_eq!(fs.stat("/link"), Err(TmpfsError::NotFound));
    }
    #[test]
    fn symlink_already_exists() {
        let mut fs = Tmpfs::new();
        fs.create_symlink("/link", "/a").unwrap();
        assert_eq!(
            fs.create_symlink("/link", "/b"),
            Err(TmpfsError::AlreadyExists)
        );
    }

    #[test]
    fn stat_symlink_not_dir() {
        let mut fs = Tmpfs::new();
        fs.create_symlink("/link", "/target").unwrap();
        let st = fs.stat("/link").unwrap();
        assert!(!st.is_dir);
        assert!(st.is_symlink);
    }

    #[test]
    fn stat_file_not_symlink() {
        let mut fs = Tmpfs::new();
        fs.create_file("/f").unwrap();
        let st = fs.stat("/f").unwrap();
        assert!(!st.is_symlink);
    }

    #[test]
    fn stat_dir_not_symlink() {
        let st = Tmpfs::new().stat("/").unwrap();
        assert!(!st.is_symlink);
    }
}
