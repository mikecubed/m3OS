//! FAT32 on-disk filesystem driver (Phase 24, Track D).
//!
//! Wraps the kernel-core parsing primitives with actual virtio-blk I/O
//! to provide a complete read/write FAT32 volume for `/data`.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use kernel_core::fs::fat32::{self, Fat32Bpb, Fat32DirEntry, Fat32Error, FAT_ENTRY_MASK, FAT_EOC};
use spin::Mutex;

/// Maximum cluster chain length before we assume corruption.
const MAX_CHAIN_LEN: usize = 65536;

// ---------------------------------------------------------------------------
// Phase 27: FAT32 permissions overlay
// ---------------------------------------------------------------------------

/// Per-file Unix metadata overlay for FAT32 (which has no native support).
#[derive(Clone, Copy)]
pub struct Fat32FileMeta {
    pub uid: u32,
    pub gid: u32,
    pub mode: u16,
}

/// In-memory permissions index for the FAT32 volume.
/// Key is the relative path within /data (e.g. "etc/passwd").
pub static FAT32_PERMISSIONS: Mutex<BTreeMap<String, Fat32FileMeta>> = Mutex::new(BTreeMap::new());

/// Get metadata for a FAT32 file, returning defaults if not in the index.
pub fn get_fat32_meta(path: &str) -> (u32, u32, u16) {
    let perms = FAT32_PERMISSIONS.lock();
    match perms.get(path) {
        Some(m) => (m.uid, m.gid, m.mode),
        None => (0, 0, 0o755), // default: root-owned, 0o755
    }
}

/// Set metadata for a FAT32 file in the permissions index.
pub fn set_fat32_meta(path: &str, uid: u32, gid: u32, mode: u16) {
    let mut perms = FAT32_PERMISSIONS.lock();
    perms.insert(String::from(path), Fat32FileMeta { uid, gid, mode });
}

/// A mounted FAT32 volume backed by virtio-blk sectors.
pub struct Fat32Volume {
    pub bpb: Fat32Bpb,
    /// Absolute LBA of the partition start on the block device.
    base_lba: u64,
    /// Absolute LBA of the first FAT sector.
    fat_start_lba: u64,
    /// Absolute LBA of the first data cluster (cluster 2).
    data_start_lba: u64,
    /// Hint for next free cluster search.
    alloc_hint: u32,
}

/// Global mounted FAT32 volume (set by mount_fat32).
pub static FAT32_VOLUME: Mutex<Option<Fat32Volume>> = Mutex::new(None);

impl Fat32Volume {
    /// Mount a FAT32 partition at the given base LBA.
    pub fn mount(base_lba: u64) -> Result<Self, Fat32Error> {
        let mut sector = vec![0u8; 512];
        crate::blk::read_sectors(base_lba, 1, &mut sector).map_err(|_| Fat32Error::IoError)?;

        let sector_array: &[u8; 512] = sector
            .as_slice()
            .try_into()
            .map_err(|_| Fat32Error::IoError)?;
        let bpb = fat32::parse_bpb(sector_array)?;

        let fat_start_lba = base_lba + bpb.reserved_sectors as u64;
        let data_start_lba = fat_start_lba + (bpb.num_fats as u64) * (bpb.fat_size_32 as u64);

        log::info!(
            "[fat32] mounted: base_lba={}, fat_start={}, data_start={}, root_cluster={}, spc={}",
            base_lba,
            fat_start_lba,
            data_start_lba,
            bpb.root_cluster,
            bpb.sectors_per_cluster
        );

        Ok(Fat32Volume {
            bpb,
            base_lba,
            fat_start_lba,
            data_start_lba,
            alloc_hint: 3,
        })
    }

    /// Convert a cluster number to an absolute disk LBA.
    fn cluster_to_lba(&self, cluster: u32) -> u64 {
        self.data_start_lba + (cluster as u64 - 2) * self.bpb.sectors_per_cluster as u64
    }

    /// Read a single FAT entry for the given cluster.
    fn read_fat_entry(&self, cluster: u32) -> Result<u32, Fat32Error> {
        let fat_offset = cluster * 4;
        let fat_sector = fat_offset / 512;
        let entry_offset = (fat_offset % 512) as usize;

        let lba = self.fat_start_lba + fat_sector as u64;
        let mut buf = vec![0u8; 512];
        crate::blk::read_sectors(lba, 1, &mut buf).map_err(|_| Fat32Error::IoError)?;

        let val = u32::from_le_bytes([
            buf[entry_offset],
            buf[entry_offset + 1],
            buf[entry_offset + 2],
            buf[entry_offset + 3],
        ]);
        Ok(val & FAT_ENTRY_MASK)
    }

    /// Write a FAT entry. Updates both FAT copies if num_fats == 2.
    fn write_fat_entry(&mut self, cluster: u32, value: u32) -> Result<(), Fat32Error> {
        let fat_offset = cluster * 4;
        let fat_sector = fat_offset / 512;
        let entry_offset = (fat_offset % 512) as usize;

        let lba = self.fat_start_lba + fat_sector as u64;
        let mut buf = vec![0u8; 512];
        crate::blk::read_sectors(lba, 1, &mut buf).map_err(|_| Fat32Error::IoError)?;

        // Preserve high 4 bits.
        let existing = u32::from_le_bytes([
            buf[entry_offset],
            buf[entry_offset + 1],
            buf[entry_offset + 2],
            buf[entry_offset + 3],
        ]);
        let new_val = (existing & 0xF000_0000) | (value & FAT_ENTRY_MASK);
        buf[entry_offset..entry_offset + 4].copy_from_slice(&new_val.to_le_bytes());

        crate::blk::write_sectors(lba, 1, &buf).map_err(|_| Fat32Error::IoError)?;

        // Update second FAT copy if present.
        if self.bpb.num_fats >= 2 {
            let lba2 = lba + self.bpb.fat_size_32 as u64;
            crate::blk::write_sectors(lba2, 1, &buf).map_err(|_| Fat32Error::IoError)?;
        }

        Ok(())
    }

    /// Walk a cluster chain from start_cluster, returning all cluster numbers.
    fn read_chain(&self, start_cluster: u32) -> Result<Vec<u32>, Fat32Error> {
        let mut chain = Vec::new();
        let mut cluster = start_cluster;

        if cluster < 2 {
            return Ok(chain);
        }

        loop {
            chain.push(cluster);
            if chain.len() > MAX_CHAIN_LEN {
                return Err(Fat32Error::ChainTooLong);
            }
            let next = self.read_fat_entry(cluster)?;
            if next == 0x0FFF_FFF7 {
                return Err(Fat32Error::InvalidCluster); // bad cluster marker
            }
            if !(2..FAT_EOC).contains(&next) {
                break; // end-of-chain or free
            }
            cluster = next;
        }

        Ok(chain)
    }

    /// Allocate a free cluster and mark it as end-of-chain.
    fn alloc_cluster(&mut self) -> Result<u32, Fat32Error> {
        let total_clusters = (self.bpb.total_sectors_32
            - (self.data_start_lba - self.base_lba) as u32)
            / self.bpb.sectors_per_cluster as u32;

        for i in 0..total_clusters {
            let candidate = ((self.alloc_hint - 2 + i) % total_clusters) + 2;
            let entry = self.read_fat_entry(candidate)?;
            if entry == 0 {
                self.write_fat_entry(candidate, FAT_EOC)?;

                // Zero the newly allocated cluster to avoid leaking stale data.
                let lba = self.cluster_to_lba(candidate);
                let size = self.bpb.sectors_per_cluster as usize * 512;
                let zero_buf = vec![0u8; size];
                crate::blk::write_sectors(lba, self.bpb.sectors_per_cluster as usize, &zero_buf)
                    .map_err(|_| Fat32Error::IoError)?;

                self.alloc_hint = candidate + 1;
                return Ok(candidate);
            }
        }

        Err(Fat32Error::DiskFull)
    }

    /// Extend a cluster chain by one cluster. Returns the newly allocated cluster.
    fn extend_chain(&mut self, last_cluster: u32) -> Result<u32, Fat32Error> {
        let new_cluster = self.alloc_cluster()?;
        self.write_fat_entry(last_cluster, new_cluster)?;
        Ok(new_cluster)
    }

    /// Read a cluster's worth of data (sectors_per_cluster * 512 bytes).
    fn read_cluster(&self, cluster: u32) -> Result<Vec<u8>, Fat32Error> {
        let lba = self.cluster_to_lba(cluster);
        let size = self.bpb.sectors_per_cluster as usize * 512;
        let mut buf = vec![0u8; size];
        crate::blk::read_sectors(lba, self.bpb.sectors_per_cluster as usize, &mut buf)
            .map_err(|_| Fat32Error::IoError)?;
        Ok(buf)
    }

    /// Write data to a cluster.
    fn write_cluster(&self, cluster: u32, data: &[u8]) -> Result<(), Fat32Error> {
        let lba = self.cluster_to_lba(cluster);
        crate::blk::write_sectors(lba, self.bpb.sectors_per_cluster as usize, data)
            .map_err(|_| Fat32Error::IoError)?;
        Ok(())
    }

    /// Read all directory entries from a directory's cluster chain.
    pub fn read_dir(&self, dir_cluster: u32) -> Result<Vec<Fat32DirEntry>, Fat32Error> {
        let chain = self.read_chain(dir_cluster)?;
        let mut entries = Vec::new();

        for &cluster in &chain {
            let data = self.read_cluster(cluster)?;
            let mut offset = 0;
            while offset + 32 <= data.len() {
                if data[offset] == 0x00 {
                    return Ok(entries); // end of directory
                }
                let raw: &[u8; 32] = data[offset..offset + 32].try_into().unwrap();
                if let Some(entry) = fat32::parse_dir_entry(raw) {
                    entries.push(entry);
                }
                offset += 32;
            }
        }

        Ok(entries)
    }

    /// Look up a path from the root directory.
    pub fn lookup(&self, path: &str) -> Result<Fat32DirEntry, Fat32Error> {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        if parts.is_empty() {
            // Root directory itself — synthesize an entry.
            return Ok(Fat32DirEntry {
                name: *b"/          ",
                attr: 0x10,
                cluster_hi: (self.bpb.root_cluster >> 16) as u16,
                cluster_lo: self.bpb.root_cluster as u16,
                file_size: 0,
            });
        }

        let mut current_cluster = self.bpb.root_cluster;

        for (i, part) in parts.iter().enumerate() {
            let entries = self.read_dir(current_cluster)?;
            let found = entries.iter().find(|e| fat32::name_matches(&e.name, part));

            match found {
                Some(entry) => {
                    if i < parts.len() - 1 {
                        // Intermediate component must be a directory.
                        if !entry.is_dir() {
                            return Err(Fat32Error::NotFound);
                        }
                        current_cluster = entry.start_cluster();
                    } else {
                        return Ok(*entry);
                    }
                }
                None => return Err(Fat32Error::NotFound),
            }
        }

        Err(Fat32Error::NotFound)
    }

    /// Read file data starting from a given offset.
    pub fn read_file(
        &self,
        start_cluster: u32,
        file_size: u32,
        offset: usize,
        buf: &mut [u8],
    ) -> Result<usize, Fat32Error> {
        if offset >= file_size as usize {
            return Ok(0); // EOF
        }

        let chain = self.read_chain(start_cluster)?;
        let cluster_size = self.bpb.sectors_per_cluster as usize * 512;
        let available = file_size as usize - offset;
        let to_read = buf.len().min(available);

        let mut bytes_read = 0;
        let mut file_pos = 0;

        for &cluster in &chain {
            let cluster_end = file_pos + cluster_size;

            if file_pos + cluster_size <= offset {
                // Skip this entire cluster.
                file_pos = cluster_end;
                continue;
            }

            let data = self.read_cluster(cluster)?;

            let start_in_cluster = offset.saturating_sub(file_pos);

            let end_in_cluster = cluster_size.min(data.len());
            let available_in_cluster = end_in_cluster - start_in_cluster;
            let copy_len = available_in_cluster.min(to_read - bytes_read);

            buf[bytes_read..bytes_read + copy_len]
                .copy_from_slice(&data[start_in_cluster..start_in_cluster + copy_len]);
            bytes_read += copy_len;

            if bytes_read >= to_read {
                break;
            }

            file_pos = cluster_end;
        }

        Ok(bytes_read)
    }

    /// Write file data. Returns (start_cluster, new_file_size).
    ///
    /// `current_size` is the file's current size; the returned size is
    /// `max(current_size, offset + data.len())` so overwrites before EOF
    /// never shrink the file.
    pub fn write_file(
        &mut self,
        start_cluster: u32,
        offset: usize,
        data: &[u8],
        current_size: usize,
    ) -> Result<(u32, usize), Fat32Error> {
        if data.is_empty() {
            let first = if start_cluster >= 2 { start_cluster } else { 0 };
            return Ok((first, current_size));
        }
        let cluster_size = self.bpb.sectors_per_cluster as usize * 512;

        // Build or extend the cluster chain as needed.
        let mut chain = if start_cluster >= 2 {
            self.read_chain(start_cluster)?
        } else {
            Vec::new()
        };

        let end_offset = offset + data.len();
        let clusters_needed = end_offset.div_ceil(cluster_size);

        // Allocate clusters if needed.
        while chain.len() < clusters_needed {
            let new_cluster = if chain.is_empty() {
                self.alloc_cluster()?
            } else {
                self.extend_chain(*chain.last().unwrap())?
            };
            chain.push(new_cluster);
        }

        let first_cluster = if chain.is_empty() { 0 } else { chain[0] };

        // Write data cluster by cluster.
        let mut data_pos = 0;
        let mut file_pos = 0;

        for &cluster in &chain {
            let cluster_end = file_pos + cluster_size;

            if cluster_end <= offset {
                file_pos = cluster_end;
                continue;
            }

            if file_pos >= end_offset {
                break;
            }

            let start_in_cluster = offset.saturating_sub(file_pos);

            let end_in_data = (end_offset - file_pos).min(cluster_size);
            let copy_len = end_in_data - start_in_cluster;

            // Read-modify-write if partial cluster.
            let mut cluster_data = if start_in_cluster > 0 || copy_len < cluster_size {
                self.read_cluster(cluster)?
            } else {
                vec![0u8; cluster_size]
            };

            cluster_data[start_in_cluster..start_in_cluster + copy_len]
                .copy_from_slice(&data[data_pos..data_pos + copy_len]);
            data_pos += copy_len;

            self.write_cluster(cluster, &cluster_data)?;

            file_pos = cluster_end;
        }

        Ok((first_cluster, end_offset.max(current_size)))
    }

    /// Create a new directory entry in the specified directory.
    fn create_entry(
        &mut self,
        dir_cluster: u32,
        name: &str,
        attr: u8,
    ) -> Result<Fat32DirEntry, Fat32Error> {
        let formatted_name = fat32::format_8_3(name);

        // Check for duplicate name before creating.
        let existing = self.read_dir(dir_cluster)?;
        if existing.iter().any(|e| fat32::name_matches(&e.name, name)) {
            return Err(Fat32Error::AlreadyExists);
        }

        let chain = self.read_chain(dir_cluster)?;
        let cluster_size = self.bpb.sectors_per_cluster as usize * 512;

        // Find a free slot in existing directory clusters.
        for &cluster in &chain {
            let mut data = self.read_cluster(cluster)?;
            let mut offset = 0;
            while offset + 32 <= data.len() {
                if data[offset] == 0x00 || data[offset] == 0xE5 {
                    // Found a free slot.
                    let mut raw = [0u8; 32];
                    raw[..11].copy_from_slice(&formatted_name);
                    raw[11] = attr;
                    // cluster_hi, cluster_lo, and file_size start as 0.
                    data[offset..offset + 32].copy_from_slice(&raw);
                    self.write_cluster(cluster, &data)?;

                    return Ok(Fat32DirEntry {
                        name: formatted_name,
                        attr,
                        cluster_hi: 0,
                        cluster_lo: 0,
                        file_size: 0,
                    });
                }
                offset += 32;
            }
        }

        // Directory is full — allocate a new cluster for it.
        let last_cluster = *chain.last().ok_or(Fat32Error::InvalidCluster)?;
        let new_cluster = self.extend_chain(last_cluster)?;
        let mut data = vec![0u8; cluster_size];

        let mut raw = [0u8; 32];
        raw[..11].copy_from_slice(&formatted_name);
        raw[11] = attr;
        data[..32].copy_from_slice(&raw);
        self.write_cluster(new_cluster, &data)?;

        Ok(Fat32DirEntry {
            name: formatted_name,
            attr,
            cluster_hi: 0,
            cluster_lo: 0,
            file_size: 0,
        })
    }

    /// Create an empty file in a directory.
    pub fn create_file(
        &mut self,
        dir_cluster: u32,
        name: &str,
    ) -> Result<Fat32DirEntry, Fat32Error> {
        self.create_entry(dir_cluster, name, 0x20) // ARCHIVE
    }

    /// Create a subdirectory.
    pub fn mkdir(&mut self, dir_cluster: u32, name: &str) -> Result<Fat32DirEntry, Fat32Error> {
        // Check for duplicate name before allocating.
        let existing = self.read_dir(dir_cluster)?;
        if existing.iter().any(|e| fat32::name_matches(&e.name, name)) {
            return Err(Fat32Error::AlreadyExists);
        }

        // Allocate a cluster for the new directory's contents.
        let new_dir_cluster = self.alloc_cluster()?;
        let cluster_size = self.bpb.sectors_per_cluster as usize * 512;
        let mut dir_data = vec![0u8; cluster_size];

        // Create "." entry.
        let mut dot = [0u8; 32];
        dot[..11].copy_from_slice(b".          ");
        dot[11] = 0x10;
        dot[20..22].copy_from_slice(&(new_dir_cluster >> 16).to_le_bytes()[..2]);
        dot[26..28].copy_from_slice(&(new_dir_cluster as u16).to_le_bytes());
        dir_data[..32].copy_from_slice(&dot);

        // Create ".." entry.
        let mut dotdot = [0u8; 32];
        dotdot[..11].copy_from_slice(b"..         ");
        dotdot[11] = 0x10;
        dotdot[20..22].copy_from_slice(&(dir_cluster >> 16).to_le_bytes()[..2]);
        dotdot[26..28].copy_from_slice(&(dir_cluster as u16).to_le_bytes());
        dir_data[32..64].copy_from_slice(&dotdot);

        self.write_cluster(new_dir_cluster, &dir_data)?;

        // Create the entry in the parent directory.
        let formatted_name = fat32::format_8_3(name);
        let chain = self.read_chain(dir_cluster)?;

        for &cluster in &chain {
            let mut data = self.read_cluster(cluster)?;
            let mut offset = 0;
            while offset + 32 <= data.len() {
                if data[offset] == 0x00 || data[offset] == 0xE5 {
                    let mut raw = [0u8; 32];
                    raw[..11].copy_from_slice(&formatted_name);
                    raw[11] = 0x10; // DIRECTORY
                    raw[20..22].copy_from_slice(&(new_dir_cluster >> 16).to_le_bytes()[..2]);
                    raw[26..28].copy_from_slice(&(new_dir_cluster as u16).to_le_bytes());
                    data[offset..offset + 32].copy_from_slice(&raw);
                    self.write_cluster(cluster, &data)?;

                    return Ok(Fat32DirEntry {
                        name: formatted_name,
                        attr: 0x10,
                        cluster_hi: (new_dir_cluster >> 16) as u16,
                        cluster_lo: new_dir_cluster as u16,
                        file_size: 0,
                    });
                }
                offset += 32;
            }
        }

        // No free slot found — free the orphaned cluster before returning.
        self.write_fat_entry(new_dir_cluster, 0)?;
        Err(Fat32Error::DirFull)
    }

    /// Delete a file (not a directory). Frees its cluster chain.
    pub fn unlink(&mut self, dir_cluster: u32, name: &str) -> Result<(), Fat32Error> {
        let formatted_name = fat32::format_8_3(name);
        let chain = self.read_chain(dir_cluster)?;

        for &cluster in &chain {
            let mut data = self.read_cluster(cluster)?;
            let mut offset = 0;
            while offset + 32 <= data.len() {
                if data[offset] == 0x00 {
                    return Err(Fat32Error::NotFound);
                }
                if data[offset] == 0xE5 {
                    offset += 32;
                    continue;
                }
                // Skip LFN entries.
                if data[offset + 11] == 0x0F {
                    offset += 32;
                    continue;
                }
                if data[offset..offset + 11] == formatted_name {
                    let attr = data[offset + 11];
                    if attr & 0x10 != 0 {
                        return Err(Fat32Error::IsDir); // Use rmdir for directories
                    }

                    // Free the cluster chain.
                    let cluster_hi =
                        u16::from_le_bytes([data[offset + 20], data[offset + 21]]) as u32;
                    let cluster_lo =
                        u16::from_le_bytes([data[offset + 26], data[offset + 27]]) as u32;
                    let start = (cluster_hi << 16) | cluster_lo;
                    if start >= 2 {
                        self.free_chain(start)?;
                    }

                    // Mark entry as deleted.
                    data[offset] = 0xE5;
                    self.write_cluster(cluster, &data)?;
                    return Ok(());
                }
                offset += 32;
            }
        }

        Err(Fat32Error::NotFound)
    }

    /// Free all clusters in a chain.
    fn free_chain(&mut self, start: u32) -> Result<(), Fat32Error> {
        let chain = self.read_chain(start)?;
        for &cluster in &chain {
            self.write_fat_entry(cluster, 0)?;
        }
        Ok(())
    }

    /// Update a directory entry's cluster and size fields.
    pub fn update_dir_entry(
        &mut self,
        dir_cluster: u32,
        name: &str,
        new_start_cluster: u32,
        new_size: u32,
    ) -> Result<(), Fat32Error> {
        let formatted_name = fat32::format_8_3(name);
        let chain = self.read_chain(dir_cluster)?;

        for &cluster in &chain {
            let mut data = self.read_cluster(cluster)?;
            let mut offset = 0;
            while offset + 32 <= data.len() {
                if data[offset] == 0x00 {
                    return Err(Fat32Error::NotFound);
                }
                if data[offset] == 0xE5 || data[offset + 11] == 0x0F {
                    offset += 32;
                    continue;
                }
                if data[offset..offset + 11] == formatted_name {
                    // Update cluster_hi.
                    data[offset + 20..offset + 22]
                        .copy_from_slice(&((new_start_cluster >> 16) as u16).to_le_bytes());
                    // Update cluster_lo.
                    data[offset + 26..offset + 28]
                        .copy_from_slice(&(new_start_cluster as u16).to_le_bytes());
                    // Update file_size.
                    data[offset + 28..offset + 32].copy_from_slice(&new_size.to_le_bytes());
                    self.write_cluster(cluster, &data)?;
                    return Ok(());
                }
                offset += 32;
            }
        }

        Err(Fat32Error::NotFound)
    }

    /// List files in a directory, returning (name, is_dir) pairs.
    pub fn list_dir(&self, dir_cluster: u32) -> Result<Vec<(String, bool)>, Fat32Error> {
        let entries = self.read_dir(dir_cluster)?;
        let mut result = Vec::new();

        for entry in &entries {
            // Skip "." and ".." for cleaner output.
            if &entry.name[..2] == b". " || &entry.name[..3] == b".. " {
                continue;
            }
            let mut name_buf = [0u8; 13];
            let len = entry.name_str(&mut name_buf);
            if let Ok(name_str) = core::str::from_utf8(&name_buf[..len]) {
                result.push((String::from(name_str), entry.is_dir()));
            }
        }

        Ok(result)
    }
}

/// Mount a FAT32 volume at the given base LBA into the global static.
pub fn mount_fat32(base_lba: u64) -> Result<(), Fat32Error> {
    let vol = Fat32Volume::mount(base_lba)?;
    *FAT32_VOLUME.lock() = Some(vol);
    log::info!("[fat32] volume mounted at base LBA {}", base_lba);
    Ok(())
}

/// Check if the FAT32 volume is mounted.
pub fn is_mounted() -> bool {
    FAT32_VOLUME.lock().is_some()
}
