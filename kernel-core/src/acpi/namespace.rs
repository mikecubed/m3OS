//! ACPI namespace: node arena, path resolution, DSDT+SSDT merge, and the
//! device queries drivers consume (Phase 101 Track B).
//!
//! The namespace is a tree of 4-character named nodes rooted at `\`. It
//! is populated by the AML load pass ([`crate::acpi::aml::interp`])
//! walking one DSDT plus any number of SSDTs into the same tree (SSDTs
//! routinely re-open scopes the DSDT defined). Definition-block bytes are
//! owned here (`Arc<[u8]>`) so method bodies can be evaluated lazily long
//! after load.
//!
//! Path resolution implements ACPI 6.5 §5.3: `\` anchors at the root,
//! each `^` hops one parent, multi-segment paths resolve strictly, and a
//! bare single segment searches the current scope then each ancestor up
//! to the root.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::aml::decode::NamePath;
use super::aml::interp::Interp;
use super::aml::object::{AmlError, AmlValue, BufferField, FieldUnit, IndexFieldUnit, RegionSpace};

/// Index into the namespace node arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeId(pub u32);

/// The namespace root (`\`).
pub const ROOT: NodeId = NodeId(0);

/// What a namespace node *is*. Data-bearing variants hold evaluated
/// values; `Method` bodies stay as byte ranges into the owning table and
/// are evaluated on demand.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeObject {
    /// A pure scope (`Scope`, and the predefined `\_SB` etc.).
    Scope,
    Device,
    Processor,
    ThermalZone,
    PowerResource,
    Mutex,
    Event,
    /// A control method: `tables[table][start..end]` is the body
    /// `TermList`. `table == u32::MAX` marks the `_OSI` built-in.
    Method {
        table: u32,
        start: u32,
        end: u32,
        arg_count: u8,
        serialized: bool,
    },
    /// A named data object (`Name`), holding its evaluated value.
    Name(AmlValue),
    /// `OperationRegion` — `offset`/`length` already evaluated at load.
    OpRegion {
        space: u8,
        offset: u64,
        length: u64,
    },
    Field(FieldUnit),
    IndexField(IndexFieldUnit),
    BufferField(BufferField),
    Alias(NodeId),
    /// `External()` declaration for a method — invocation parses
    /// `arg_count` args and yields `Integer(0)`.
    ExternalMethod {
        arg_count: u8,
    },
    /// `External()` declaration for a non-method object.
    External,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub seg: [u8; 4],
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub object: NodeObject,
}

/// Result summary of one table load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadSummary {
    /// Packages (Device/Scope/… bodies) skipped by the tolerant loader,
    /// as (path-at-error, error) pairs. Empty = clean load.
    pub skipped: Vec<(String, AmlError)>,
}

pub struct Namespace {
    nodes: Vec<Node>,
    /// Owned definition blocks; method byte ranges index into these.
    pub(crate) tables: Vec<Arc<[u8]>>,
    /// `Notify(device, code)` events recorded during evaluation, drained
    /// by the event dispatcher (`acpid`, Track D/E).
    pub pending_notify: Vec<(NodeId, u64)>,
}

impl Default for Namespace {
    fn default() -> Self {
        Self::new()
    }
}

impl Namespace {
    /// A namespace holding the root and the predefined scopes/objects of
    /// ACPI 6.5 §5.3.1: `\_SB`, `\_SI`, `\_TZ`, `\_GPE`, `\_PR`, the
    /// global-lock mutex `\_GL`, `\_REV`, `\_OS`, and the `\_OSI`
    /// built-in method.
    pub fn new() -> Namespace {
        let mut ns = Namespace {
            nodes: alloc::vec![Node {
                seg: *b"\\___",
                parent: None,
                children: Vec::new(),
                object: NodeObject::Scope,
            }],
            tables: Vec::new(),
            pending_notify: Vec::new(),
        };
        for scope in [b"_SB_", b"_SI_", b"_TZ_", b"_GPE", b"_PR_"] {
            ns.attach(ROOT, *scope, NodeObject::Scope);
        }
        ns.attach(ROOT, *b"_GL_", NodeObject::Mutex);
        // \_REV: 2 = ACPI 2.0+ 64-bit integer arithmetic (what every
        // modern interpreter reports).
        ns.attach(ROOT, *b"_REV", NodeObject::Name(AmlValue::Integer(2)));
        // \_OS: firmware compares this against Windows strings; answering
        // as Windows (the ACPICA/Linux default posture) keeps DSDT device
        // paths on the tested-by-the-vendor branches.
        ns.attach(
            ROOT,
            *b"_OS_",
            NodeObject::Name(AmlValue::String(String::from("Microsoft Windows NT"))),
        );
        ns.attach(
            ROOT,
            *b"_OSI",
            NodeObject::Method {
                table: u32::MAX,
                start: 0,
                end: 0,
                arg_count: 1,
                serialized: false,
            },
        );
        ns
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    /// Direct child of `parent` named `seg`.
    pub fn child(&self, parent: NodeId, seg: [u8; 4]) -> Option<NodeId> {
        self.node(parent)
            .children
            .iter()
            .copied()
            .find(|&c| self.node(c).seg == seg)
    }

    /// Attach a new node, or overwrite the object of an existing
    /// same-named child. Re-opening semantics: if the existing child is a
    /// scope-bearing node and the new object is `Scope`, the existing
    /// node (and its children) is kept as-is — that is how an SSDT
    /// re-opens `\_SB.PCI0`. Otherwise the last definition wins (tolerant
    /// of firmware redefinition).
    pub(crate) fn attach(&mut self, parent: NodeId, seg: [u8; 4], object: NodeObject) -> NodeId {
        if let Some(existing) = self.child(parent, seg) {
            // `Scope` re-opens an existing node without disturbing what it
            // is (that is how an SSDT extends `\_SB.PCI0`); anything else
            // is a redefinition and the last definition wins — including a
            // real definition replacing an `External` declaration.
            let reopen = matches!(object, NodeObject::Scope)
                && !matches!(
                    self.node(existing).object,
                    NodeObject::External | NodeObject::ExternalMethod { .. }
                );
            if !reopen {
                self.node_mut(existing).object = object;
            }
            return existing;
        }
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Node {
            seg,
            parent: Some(parent),
            children: Vec::new(),
            object,
        });
        self.node_mut(parent).children.push(id);
        id
    }

    /// Follow `Alias` links to the underlying node (bounded — alias
    /// chains in real firmware are length 1).
    pub fn deref_alias(&self, mut id: NodeId) -> NodeId {
        for _ in 0..8 {
            match self.node(id).object {
                NodeObject::Alias(t) => id = t,
                _ => break,
            }
        }
        id
    }

    /// Resolve `path` against `scope` per §5.3 (single-segment upward
    /// search; strict multi-segment walk; alias-transparent).
    pub fn resolve(&self, scope: NodeId, path: &NamePath) -> Option<NodeId> {
        let mut base = if path.root { ROOT } else { scope };
        for _ in 0..path.parent_hops {
            base = self.node(base).parent?;
        }
        match path.segs.len() {
            0 => Some(base),
            1 if !path.root && path.parent_hops == 0 => {
                // Search rules: current scope, then ancestors.
                let seg = path.segs[0];
                let mut cur = Some(base);
                while let Some(c) = cur {
                    if let Some(hit) = self.child(c, seg) {
                        return Some(self.deref_alias(hit));
                    }
                    cur = self.node(c).parent;
                }
                None
            }
            _ => {
                let mut cur = base;
                for seg in &path.segs {
                    cur = self.deref_alias(self.child(cur, *seg)?);
                }
                Some(cur)
            }
        }
    }

    /// Resolve the *container* of `path` and create (or overwrite) its
    /// final segment with `object`. The container must already exist.
    pub(crate) fn create_path(
        &mut self,
        scope: NodeId,
        path: &NamePath,
        object: NodeObject,
    ) -> Result<NodeId, AmlError> {
        let last = *path.segs.last().ok_or(AmlError::BadNameString)?;
        let container = if path.segs.len() == 1 && !path.root && path.parent_hops == 0 {
            scope
        } else {
            let container_path = NamePath {
                root: path.root,
                parent_hops: path.parent_hops,
                segs: path.segs[..path.segs.len() - 1].to_vec(),
            };
            self.resolve(scope, &container_path)
                .ok_or(AmlError::BadScope)?
        };
        Ok(self.attach(container, last, object))
    }

    /// Full ASL-style path of a node (`\_SB.PCI0.I2C1.TPD0`), for
    /// diagnostics.
    pub fn full_path(&self, id: NodeId) -> String {
        if id == ROOT {
            return String::from("\\");
        }
        let mut segs: Vec<[u8; 4]> = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            if c == ROOT {
                break;
            }
            segs.push(self.node(c).seg);
            cur = self.node(c).parent;
        }
        let mut s = String::from("\\");
        for (i, seg) in segs.iter().rev().enumerate() {
            if i > 0 {
                s.push('.');
            }
            // NameSegs are stored `_`-padded to 4 bytes (`_SB_`); render
            // the ASL convention (`_SB`) by trimming the padding.
            let len = seg.iter().rposition(|&b| b != b'_').map_or(1, |p| p + 1);
            for &b in &seg[..len] {
                s.push(b as char);
            }
        }
        s
    }

    /// Every `Device` node (plus `Processor`/`ThermalZone`, which behave
    /// as device scopes for enumeration), in arena order.
    pub fn devices(&self) -> Vec<NodeId> {
        (0..self.nodes.len() as u32)
            .map(NodeId)
            .filter(|&id| {
                matches!(
                    self.node(id).object,
                    NodeObject::Device | NodeObject::Processor | NodeObject::ThermalZone
                )
            })
            .collect()
    }

    /// Phase 103 C — only the `ThermalZone` nodes, in arena order (the
    /// zone-enumeration surface behind acpid's `ACPI_LIST_TZ` verb).
    pub fn thermal_zones(&self) -> Vec<NodeId> {
        (0..self.nodes.len() as u32)
            .map(NodeId)
            .filter(|&id| matches!(self.node(id).object, NodeObject::ThermalZone))
            .collect()
    }

    /// Phase 103 B — devices carrying a `_BCL` child (backlight-capable
    /// display outputs; the surface behind acpid's `ACPI_LIST_BACKLIGHT`
    /// verb). QEMU q35 declares none — the populated path is covered by
    /// the synthetic-fixture host test.
    pub fn backlight_devices(&self) -> Vec<NodeId> {
        self.devices()
            .into_iter()
            .filter(|&dev| self.child(dev, *b"_BCL").is_some())
            .collect()
    }

    // -- Track B query surface -------------------------------------------

    /// Load one definition block (DSDT or SSDT): validates the 36-byte
    /// SDT header, takes ownership of the bytes, and runs the AML load
    /// pass, merging into this namespace. Skipped packages (tolerant
    /// mode) are reported in the summary.
    pub fn load_table<R: RegionSpace>(
        &mut self,
        bytes: &[u8],
        regions: &mut R,
    ) -> Result<LoadSummary, AmlError> {
        if bytes.len() < super::aml::decode::ACPI_SDT_HEADER_LEN {
            return Err(AmlError::Truncated);
        }
        // Declared length must match the buffer we were handed.
        let declared = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        if declared > bytes.len() || declared < super::aml::decode::ACPI_SDT_HEADER_LEN {
            return Err(AmlError::Truncated);
        }
        let table: Arc<[u8]> = Arc::from(&bytes[..declared]);
        self.tables.push(table);
        let idx = self.tables.len() - 1;
        let mut interp = Interp::new(self, regions);
        interp.load_table(idx)
    }

    /// Evaluate the object at `path` (a method is invoked with no args;
    /// a `Name`/field is read).
    pub fn evaluate<R: RegionSpace>(
        &mut self,
        regions: &mut R,
        path: &str,
    ) -> Result<AmlValue, AmlError> {
        self.evaluate_with_args(regions, path, Vec::new())
    }

    /// Phase 103 B — evaluate a method with explicit arguments (`_BCM`
    /// takes the target brightness level as Arg0). Non-method nodes
    /// ignore the args and read as in [`Self::evaluate`].
    pub fn evaluate_with_args<R: RegionSpace>(
        &mut self,
        regions: &mut R,
        path: &str,
        args: Vec<AmlValue>,
    ) -> Result<AmlValue, AmlError> {
        let node = self.resolve_str(path).ok_or(AmlError::UnresolvedName)?;
        let mut interp = Interp::new(self, regions);
        interp.evaluate_node(node, args)
    }

    /// Resolve an ASL-style textual path (`\_SB.PCI0.I2C1`) — test and
    /// query convenience; absolute paths only.
    pub fn resolve_str(&self, path: &str) -> Option<NodeId> {
        let p = path.strip_prefix('\\')?;
        let mut cur = ROOT;
        if p.is_empty() {
            return Some(cur);
        }
        for seg in p.split('.') {
            let b = seg.as_bytes();
            if b.is_empty() || b.len() > 4 {
                return None;
            }
            let mut padded = [b'_'; 4];
            padded[..b.len()].copy_from_slice(b);
            cur = self.deref_alias(self.child(cur, padded)?);
        }
        Some(cur)
    }

    /// All present devices whose `_HID` or `_CID` matches `hid` (string
    /// form — `EisaId`-encoded integers are decoded before comparison).
    /// Presence is `_STA` bit 0 (absent `_STA` ⇒ present, §6.3.7).
    pub fn find_by_hid<R: RegionSpace>(&mut self, regions: &mut R, hid: &str) -> Vec<NodeId> {
        let mut out = Vec::new();
        for dev in self.devices() {
            let matched = {
                let mut interp = Interp::new(self, regions);
                interp.device_matches_id(dev, hid)
            };
            if !matched {
                continue;
            }
            let present = {
                let mut interp = Interp::new(self, regions);
                interp.sta(dev) & 1 != 0
            };
            if present {
                out.push(dev);
            }
        }
        out
    }

    /// `_STA` of a device (0x0F when absent, per spec).
    pub fn sta<R: RegionSpace>(&mut self, regions: &mut R, device: NodeId) -> u64 {
        let mut interp = Interp::new(self, regions);
        interp.sta(device)
    }

    /// Evaluate a device's `_CRS` to its resource-template buffer bytes
    /// (feed to [`crate::acpi::resource::decode_crs`]).
    pub fn crs_bytes<R: RegionSpace>(
        &mut self,
        regions: &mut R,
        device: NodeId,
    ) -> Result<Vec<u8>, AmlError> {
        let crs = self
            .child(device, *b"_CRS")
            .map(|n| self.deref_alias(n))
            .ok_or(AmlError::UnresolvedName)?;
        let mut interp = Interp::new(self, regions);
        match interp.evaluate_node(crs, Vec::new())? {
            AmlValue::Buffer(b) => Ok(b),
            _ => Err(AmlError::TypeMismatch),
        }
    }

    /// One-call driver query: evaluate a device's `_CRS` and decode it —
    /// "what bus / slave address / IRQ / GPIO is device X on?" answered
    /// as a populated [`crate::acpi::resource::DeviceResources`].
    pub fn device_resources<R: RegionSpace>(
        &mut self,
        regions: &mut R,
        device: NodeId,
    ) -> Result<crate::acpi::resource::DeviceResources, AmlError> {
        let bytes = self.crs_bytes(regions, device)?;
        crate::acpi::resource::decode_crs(&bytes)
    }
}

/// Decode an `EisaId`-encoded 32-bit `_HID`/`_CID` into its 7-character
/// text form (`0x0C0CD041` → `"PNP0C0C"`).
pub fn eisa_id_decode(v: u32) -> String {
    let b = v.to_le_bytes();
    let c1 = ((b[0] >> 2) & 0x1F) + 0x40;
    let c2 = (((b[0] & 0x03) << 3) | (b[1] >> 5)) + 0x40;
    let c3 = (b[1] & 0x1F) + 0x40;
    let hex = |n: u8| -> u8 { if n < 10 { b'0' + n } else { b'A' + n - 10 } };
    let mut s = String::with_capacity(7);
    s.push(c1 as char);
    s.push(c2 as char);
    s.push(c3 as char);
    s.push(hex(b[2] >> 4) as char);
    s.push(hex(b[2] & 0xF) as char);
    s.push(hex(b[3] >> 4) as char);
    s.push(hex(b[3] & 0xF) as char);
    s
}

/// Encode a 7-character EISA ID text form back to its 32-bit value.
/// Returns `None` for anything that is not `[A-Z]{3}[0-9A-Fa-f]{4}`.
pub fn eisa_id_encode(s: &str) -> Option<u32> {
    let b = s.as_bytes();
    if b.len() != 7 {
        return None;
    }
    let letter = |c: u8| -> Option<u8> {
        if c.is_ascii_uppercase() {
            Some(c - 0x40)
        } else {
            None
        }
    };
    let hex = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'A'..=b'F' => Some(c - b'A' + 10),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    };
    let (c1, c2, c3) = (letter(b[0])?, letter(b[1])?, letter(b[2])?);
    let (h1, h2, h3, h4) = (hex(b[3])?, hex(b[4])?, hex(b[5])?, hex(b[6])?);
    let bytes = [
        (c1 << 2) | (c2 >> 3),
        ((c2 & 0x07) << 5) | c3,
        (h1 << 4) | h2,
        (h3 << 4) | h4,
    ];
    Some(u32::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eisa_round_trip() {
        // The three laptop staples the charter names.
        for (id, text) in [
            (0x0A0CD041u32, "PNP0C0A"), // battery
            (0x0D0CD041u32, "PNP0C0D"), // lid
            (0x0C0CD041u32, "PNP0C0C"), // power button
            (0x030AD041u32, "PNP0A03"), // PCI bus (known-good reference)
        ] {
            assert_eq!(eisa_id_decode(id), text);
            assert_eq!(eisa_id_encode(text), Some(id));
        }
        assert_eq!(eisa_id_encode("pnp0c0c"), None);
        assert_eq!(eisa_id_encode("PNP0C0"), None);
    }

    #[test]
    fn predefined_scopes_exist() {
        let ns = Namespace::new();
        for p in [
            "\\_SB", "\\_TZ", "\\_GPE", "\\_PR", "\\_SI", "\\_GL", "\\_REV", "\\_OS", "\\_OSI",
        ] {
            assert!(ns.resolve_str(p).is_some(), "{p} missing");
        }
    }

    #[test]
    fn resolve_search_rules() {
        let mut ns = Namespace::new();
        let sb = ns.resolve_str("\\_SB").unwrap();
        let pci = ns.attach(sb, *b"PCI0", NodeObject::Device);
        let i2c = ns.attach(pci, *b"I2C1", NodeObject::Device);
        ns.attach(sb, *b"GLOB", NodeObject::Name(AmlValue::Integer(9)));

        // Single-seg upward search from a nested scope finds \_SB.GLOB.
        let path = NamePath {
            root: false,
            parent_hops: 0,
            segs: alloc::vec![*b"GLOB"],
        };
        let hit = ns.resolve(i2c, &path).unwrap();
        assert_eq!(ns.full_path(hit), "\\_SB.GLOB");

        // Multi-seg paths do NOT search upward.
        let path = NamePath {
            root: false,
            parent_hops: 0,
            segs: alloc::vec![*b"PCI0", *b"GLOB"],
        };
        assert!(ns.resolve(i2c, &path).is_none());

        // Parent hops.
        let path = NamePath {
            root: false,
            parent_hops: 1,
            segs: alloc::vec![*b"I2C1"],
        };
        assert_eq!(ns.resolve(i2c, &path), Some(i2c));

        assert_eq!(ns.full_path(i2c), "\\_SB.PCI0.I2C1");
    }

    #[test]
    fn alias_and_reopen() {
        let mut ns = Namespace::new();
        let sb = ns.resolve_str("\\_SB").unwrap();
        let dev = ns.attach(sb, *b"DEV0", NodeObject::Device);
        ns.attach(sb, *b"ALI0", NodeObject::Alias(dev));
        assert_eq!(ns.resolve_str("\\_SB.ALI0"), Some(dev));
        // Re-opening \_SB.DEV0 as a Scope keeps the Device object.
        let again = ns.attach(sb, *b"DEV0", NodeObject::Scope);
        assert_eq!(again, dev);
        assert!(matches!(ns.node(dev).object, NodeObject::Device));
    }
}
