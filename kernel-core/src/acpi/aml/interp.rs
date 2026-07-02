//! The AML evaluator: one tree-walking engine serves both the table
//! *load pass* (namespace population — `Scope`/`Device`/`Method`/`Name`/
//! `OperationRegion`/`Field` declarations, plus any module-level
//! executable code) and on-demand *control-method evaluation*
//! (`_STA`/`_HID`/`_CRS`/GPE methods).
//!
//! # Safety bounds (untrusted firmware bytecode)
//!
//! - [`MAX_DEPTH`] bounds combined expression/method recursion.
//! - [`MAX_OPS`] bounds total executed terms (runaway `While`).
//! - [`MAX_BUFFER`] / [`MAX_PACKAGE`] bound hostile allocations.
//! - Malformed input returns [`AmlError`]; nothing here panics.
//!
//! # Load tolerance
//!
//! Real firmware AML routinely contains vendor code outside any
//! pragmatic subset. During the load pass, a term that fails inside a
//! skippable package (a `Device`/`Scope`/`Method`/`If`/… body whose
//! `PkgLength` extent is known) is skipped and recorded in the
//! [`LoadSummary`] rather than aborting the whole table; method
//! evaluation is strict.

use alloc::string::String;
use alloc::vec::Vec;

use super::decode::{self, Stream};
use super::object::{
    AccessType, AmlError, AmlValue, BufferField, FieldUnit, IndexFieldUnit, RegionSpace, Target,
};
use crate::acpi::namespace::{LoadSummary, Namespace, NodeId, NodeObject, ROOT};

/// Combined recursion bound: expression nesting + method call depth.
/// Each AML level costs several native stack frames (evaluator →
/// term-list → expression), so this is sized for a ~64 KiB-conservative
/// native stack, not for what firmware "should" do — real DSDT call
/// chains are ~10 deep and expression nests shallower still.
pub const MAX_DEPTH: u32 = 64;
/// Total term/expression budget per top-level entry (load or evaluate).
pub const MAX_OPS: u64 = 2_000_000;
/// Largest `Buffer` the interpreter will allocate (hostile-AML guard).
pub const MAX_BUFFER: usize = 1 << 22;
/// Largest `Package` element count (hostile-AML guard).
pub const MAX_PACKAGE: usize = 4096;

/// AML `Ones` under 64-bit (ComplianceRevision ≥ 2) arithmetic.
const ONES: u64 = u64::MAX;

/// Per-method execution context.
struct Frame {
    locals: [AmlValue; 8],
    args: Vec<AmlValue>,
    /// Scope for name creation/resolution: the method node during method
    /// execution, the enclosing `Device`/`Scope` during load.
    scope: NodeId,
}

impl Frame {
    fn new(scope: NodeId) -> Frame {
        Frame {
            locals: core::array::from_fn(|_| AmlValue::Uninitialized),
            args: Vec::new(),
            scope,
        }
    }
}

/// Statement-level control flow.
enum Flow {
    Normal,
    Return(AmlValue),
    Break,
    Continue,
}

pub struct Interp<'a, R: RegionSpace> {
    pub ns: &'a mut Namespace,
    pub regions: &'a mut R,
    depth: u32,
    ops: u64,
    /// Table index method bodies created right now belong to.
    current_table: u32,
    /// True during the table load pass (enables per-package skip
    /// tolerance); false during method evaluation (strict).
    in_load: bool,
    skipped: Vec<(String, AmlError)>,
}

impl<'a, R: RegionSpace> Interp<'a, R> {
    pub fn new(ns: &'a mut Namespace, regions: &'a mut R) -> Interp<'a, R> {
        Interp {
            ns,
            regions,
            depth: 0,
            ops: 0,
            current_table: u32::MAX,
            in_load: false,
            skipped: Vec::new(),
        }
    }

    fn tick(&mut self) -> Result<(), AmlError> {
        self.ops += 1;
        if self.ops > MAX_OPS {
            return Err(AmlError::LoopLimit);
        }
        Ok(())
    }

    fn guarded<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, AmlError>,
    ) -> Result<T, AmlError> {
        if self.depth >= MAX_DEPTH {
            return Err(AmlError::RecursionLimit);
        }
        self.depth += 1;
        let r = f(self);
        self.depth -= 1;
        r
    }

    // -----------------------------------------------------------------
    // Entry points
    // -----------------------------------------------------------------

    /// Run the load pass over `ns.tables[table_idx]` (header already
    /// validated by [`Namespace::load_table`]).
    pub fn load_table(&mut self, table_idx: usize) -> Result<LoadSummary, AmlError> {
        let table = self
            .ns
            .tables
            .get(table_idx)
            .cloned()
            .ok_or(AmlError::Malformed)?;
        let mut st = Stream::new(&table);
        st.seek(decode::ACPI_SDT_HEADER_LEN)?;
        let end = table.len();
        let prev = self.current_table;
        let prev_load = self.in_load;
        self.current_table = table_idx as u32;
        self.in_load = true;
        let mut frame = Frame::new(ROOT);
        let result = self.exec_term_list(&mut st, end, &mut frame, true);
        self.current_table = prev;
        self.in_load = prev_load;
        result?;
        Ok(LoadSummary {
            skipped: core::mem::take(&mut self.skipped),
        })
    }

    /// Evaluate a node: methods are invoked with `args`; data objects and
    /// fields are read.
    pub fn evaluate_node(
        &mut self,
        node: NodeId,
        args: Vec<AmlValue>,
    ) -> Result<AmlValue, AmlError> {
        let node = self.ns.deref_alias(node);
        match self.ns.node(node).object {
            NodeObject::Method { .. } => self.invoke_method(node, args),
            NodeObject::ExternalMethod { .. } => Ok(AmlValue::Integer(0)),
            _ => self.read_node(node),
        }
    }

    /// `_STA` of `device`: absent `_STA` means present+enabled (0x0F,
    /// §6.3.7); an `_STA` our subset cannot evaluate is treated the same
    /// way (enumeration should not hide devices behind interpreter gaps).
    pub fn sta(&mut self, device: NodeId) -> u64 {
        match self.ns.child(device, *b"_STA") {
            None => 0x0F,
            Some(n) => {
                let n = self.ns.deref_alias(n);
                self.evaluate_node(n, Vec::new())
                    .and_then(|v| v.as_integer())
                    .unwrap_or(0x0F)
            }
        }
    }

    /// Does `device`'s `_HID` or `_CID` match `id` (text form)?
    pub fn device_matches_id(&mut self, device: NodeId, id: &str) -> bool {
        let matches_one = |v: &AmlValue| -> bool {
            match v {
                AmlValue::Integer(i) => crate::acpi::namespace::eisa_id_decode(*i as u32) == id,
                AmlValue::String(s) => s == id,
                _ => false,
            }
        };
        for seg in [*b"_HID", *b"_CID"] {
            let Some(n) = self.ns.child(device, seg) else {
                continue;
            };
            let n = self.ns.deref_alias(n);
            let Ok(v) = self.evaluate_node(n, Vec::new()) else {
                continue;
            };
            let hit = match &v {
                AmlValue::Package(elems) => elems.iter().any(matches_one),
                other => matches_one(other),
            };
            if hit {
                return true;
            }
        }
        false
    }

    // -----------------------------------------------------------------
    // Statement execution
    // -----------------------------------------------------------------

    fn exec_term_list(
        &mut self,
        st: &mut Stream,
        end: usize,
        frame: &mut Frame,
        tolerant: bool,
    ) -> Result<Flow, AmlError> {
        while st.pos() < end {
            let term_start = st.pos();
            match self.exec_term(st, frame) {
                Ok(Flow::Normal) => {}
                Ok(flow) => return Ok(flow),
                Err(e) if tolerant => {
                    if self.skip_package_term(st, term_start, end).is_some() {
                        let at = self.ns.full_path(frame.scope);
                        self.skipped.push((at, e));
                    } else {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(Flow::Normal)
    }

    /// If the term starting at `term_start` carries a `PkgLength` extent,
    /// reposition the stream past it (load-tolerance skip).
    fn skip_package_term(&mut self, st: &mut Stream, term_start: usize, end: usize) -> Option<()> {
        st.seek(term_start).ok()?;
        let op = st.next_u8().ok()?;
        match op {
            decode::SCOPE_OP
            | decode::METHOD_OP
            | decode::IF_OP
            | decode::WHILE_OP
            | decode::BUFFER_OP
            | decode::PACKAGE_OP
            | decode::VAR_PACKAGE_OP => {}
            decode::EXT_OP_PREFIX => {
                let ext = st.next_u8().ok()?;
                match ext {
                    decode::EXT_DEVICE_OP
                    | decode::EXT_PROCESSOR_OP
                    | decode::EXT_POWER_RES_OP
                    | decode::EXT_THERMAL_ZONE_OP
                    | decode::EXT_FIELD_OP
                    | decode::EXT_INDEX_FIELD_OP
                    | decode::EXT_BANK_FIELD_OP => {}
                    _ => return None,
                }
            }
            _ => return None,
        }
        let pkg_end = st.pkg_end().ok()?;
        if pkg_end > end {
            return None;
        }
        st.seek(pkg_end).ok()?;
        Some(())
    }

    fn exec_term(&mut self, st: &mut Stream, frame: &mut Frame) -> Result<Flow, AmlError> {
        self.tick()?;
        let op = st.peek().ok_or(AmlError::Truncated)?;
        match op {
            decode::NAME_OP => {
                st.next_u8()?;
                let path = st.name_string()?;
                let value = self.eval_data_ref_object(st, frame)?;
                self.ns
                    .create_path(frame.scope, &path, NodeObject::Name(value))?;
                Ok(Flow::Normal)
            }
            decode::ALIAS_OP => {
                st.next_u8()?;
                let src = st.name_string()?;
                let alias = st.name_string()?;
                let target = self
                    .ns
                    .resolve(frame.scope, &src)
                    .ok_or(AmlError::UnresolvedName)?;
                self.ns
                    .create_path(frame.scope, &alias, NodeObject::Alias(target))?;
                Ok(Flow::Normal)
            }
            decode::SCOPE_OP => {
                st.next_u8()?;
                let end = st.pkg_end()?;
                let path = st.name_string()?;
                // Per spec the scope must exist; real SSDTs sometimes
                // open scopes for devices from tables we have not seen,
                // so create missing path heads tolerantly.
                let node = match self.ns.resolve(frame.scope, &path) {
                    Some(n) => n,
                    None => self.ns.create_path(frame.scope, &path, NodeObject::Scope)?,
                };
                self.exec_body(st, end, node, frame)
            }
            decode::METHOD_OP => {
                st.next_u8()?;
                let end = st.pkg_end()?;
                let path = st.name_string()?;
                let flags = st.next_u8()?;
                let body_start = st.pos();
                self.ns.create_path(
                    frame.scope,
                    &path,
                    NodeObject::Method {
                        table: self.current_table,
                        start: body_start as u32,
                        end: end as u32,
                        arg_count: flags & 0x07,
                        serialized: flags & 0x08 != 0,
                    },
                )?;
                st.seek(end)?;
                Ok(Flow::Normal)
            }
            decode::EXTERNAL_OP => {
                st.next_u8()?;
                let path = st.name_string()?;
                let obj_type = st.next_u8()?;
                let argc = st.next_u8()?;
                let object = if obj_type == 8 {
                    NodeObject::ExternalMethod {
                        arg_count: argc & 0x07,
                    }
                } else {
                    NodeObject::External
                };
                self.ns.create_path(frame.scope, &path, object)?;
                Ok(Flow::Normal)
            }
            decode::IF_OP => self.exec_if(st, frame),
            decode::WHILE_OP => self.exec_while(st, frame),
            decode::RETURN_OP => {
                st.next_u8()?;
                let v = self.eval_term_arg(st, frame)?;
                Ok(Flow::Return(v))
            }
            decode::BREAK_OP => {
                st.next_u8()?;
                Ok(Flow::Break)
            }
            decode::CONTINUE_OP => {
                st.next_u8()?;
                Ok(Flow::Continue)
            }
            decode::NOOP_OP | decode::BREAK_POINT_OP => {
                st.next_u8()?;
                Ok(Flow::Normal)
            }
            decode::ELSE_OP => {
                // An Else not consumed by exec_if is malformed.
                Err(AmlError::Malformed)
            }
            decode::EXT_OP_PREFIX => {
                let ext = st.peek_at(1).ok_or(AmlError::Truncated)?;
                match ext {
                    decode::EXT_DEVICE_OP | decode::EXT_THERMAL_ZONE_OP => {
                        st.take(2)?;
                        let end = st.pkg_end()?;
                        let path = st.name_string()?;
                        let object = if ext == decode::EXT_DEVICE_OP {
                            NodeObject::Device
                        } else {
                            NodeObject::ThermalZone
                        };
                        let node = self.ns.create_path(frame.scope, &path, object)?;
                        self.exec_body(st, end, node, frame)
                    }
                    decode::EXT_PROCESSOR_OP => {
                        st.take(2)?;
                        let end = st.pkg_end()?;
                        let path = st.name_string()?;
                        let _proc_id = st.next_u8()?;
                        let _pblk_addr = st.next_u32()?;
                        let _pblk_len = st.next_u8()?;
                        let node =
                            self.ns
                                .create_path(frame.scope, &path, NodeObject::Processor)?;
                        self.exec_body(st, end, node, frame)
                    }
                    decode::EXT_POWER_RES_OP => {
                        st.take(2)?;
                        let end = st.pkg_end()?;
                        let path = st.name_string()?;
                        let _system_level = st.next_u8()?;
                        let _resource_order = st.next_u16()?;
                        let node =
                            self.ns
                                .create_path(frame.scope, &path, NodeObject::PowerResource)?;
                        self.exec_body(st, end, node, frame)
                    }
                    decode::EXT_OP_REGION_OP => {
                        st.take(2)?;
                        let path = st.name_string()?;
                        let space = st.next_u8()?;
                        let offset = self.eval_int(st, frame)?;
                        let length = self.eval_int(st, frame)?;
                        self.ns.create_path(
                            frame.scope,
                            &path,
                            NodeObject::OpRegion {
                                space,
                                offset,
                                length,
                            },
                        )?;
                        Ok(Flow::Normal)
                    }
                    decode::EXT_FIELD_OP => {
                        st.take(2)?;
                        let end = st.pkg_end()?;
                        let region_path = st.name_string()?;
                        let region = self
                            .ns
                            .resolve(frame.scope, &region_path)
                            .ok_or(AmlError::UnresolvedName)?;
                        let flags = st.next_u8()?;
                        self.field_list(st, end, frame, flags, |access, off, len| {
                            NodeObject::Field(FieldUnit {
                                region,
                                bit_offset: off,
                                bit_len: len,
                                access,
                            })
                        })
                    }
                    decode::EXT_INDEX_FIELD_OP => {
                        st.take(2)?;
                        let end = st.pkg_end()?;
                        let index_path = st.name_string()?;
                        let data_path = st.name_string()?;
                        let index = self
                            .ns
                            .resolve(frame.scope, &index_path)
                            .ok_or(AmlError::UnresolvedName)?;
                        let data = self
                            .ns
                            .resolve(frame.scope, &data_path)
                            .ok_or(AmlError::UnresolvedName)?;
                        let flags = st.next_u8()?;
                        self.field_list(st, end, frame, flags, |access, off, len| {
                            NodeObject::IndexField(IndexFieldUnit {
                                index,
                                data,
                                bit_offset: off,
                                bit_len: len,
                                access,
                            })
                        })
                    }
                    decode::EXT_MUTEX_OP => {
                        st.take(2)?;
                        let path = st.name_string()?;
                        let _sync = st.next_u8()?;
                        self.ns.create_path(frame.scope, &path, NodeObject::Mutex)?;
                        Ok(Flow::Normal)
                    }
                    decode::EXT_EVENT_OP => {
                        st.take(2)?;
                        let path = st.name_string()?;
                        self.ns.create_path(frame.scope, &path, NodeObject::Event)?;
                        Ok(Flow::Normal)
                    }
                    decode::EXT_BANK_FIELD_OP => {
                        Err(AmlError::UnsupportedOpcode(0x8000 | ext as u16))
                    }
                    // Everything else under 0x5B is an expression-level
                    // opcode; evaluate and discard.
                    _ => {
                        let _ = self.eval_term_arg(st, frame)?;
                        Ok(Flow::Normal)
                    }
                }
            }
            // Any other term is an expression statement (Store, method
            // invocation, Notify, …): evaluate and discard the value.
            _ => {
                let _ = self.eval_term_arg(st, frame)?;
                Ok(Flow::Normal)
            }
        }
    }

    /// Execute a `Device`/`Scope`/… body with `scope` as the naming
    /// context, then restore the previous scope and stream position.
    fn exec_body(
        &mut self,
        st: &mut Stream,
        end: usize,
        scope: NodeId,
        frame: &mut Frame,
    ) -> Result<Flow, AmlError> {
        let prev = frame.scope;
        frame.scope = scope;
        // Device/Scope bodies stay skip-tolerant throughout the load pass
        // (so one bad nested package cannot take down a whole subtree);
        // method bodies are strict.
        let tolerant = self.in_load;
        let r = self.guarded(|s| s.exec_term_list(st, end, frame, tolerant));
        frame.scope = prev;
        let flow = r?;
        st.seek(end)?;
        Ok(flow)
    }

    fn exec_if(&mut self, st: &mut Stream, frame: &mut Frame) -> Result<Flow, AmlError> {
        st.next_u8()?;
        let end = st.pkg_end()?;
        let pred = self.eval_int(st, frame)?;
        let mut flow = Flow::Normal;
        if pred != 0 {
            flow = self.guarded(|s| s.exec_term_list(st, end, frame, false))?;
        }
        st.seek(end)?;
        if st.peek() == Some(decode::ELSE_OP) {
            st.next_u8()?;
            let else_end = st.pkg_end()?;
            if pred == 0 {
                flow = self.guarded(|s| s.exec_term_list(st, else_end, frame, false))?;
            }
            st.seek(else_end)?;
        }
        Ok(flow)
    }

    fn exec_while(&mut self, st: &mut Stream, frame: &mut Frame) -> Result<Flow, AmlError> {
        st.next_u8()?;
        let end = st.pkg_end()?;
        let pred_pos = st.pos();
        loop {
            self.tick()?;
            st.seek(pred_pos)?;
            let pred = self.eval_int(st, frame)?;
            if pred == 0 {
                break;
            }
            match self.guarded(|s| s.exec_term_list(st, end, frame, false))? {
                Flow::Normal | Flow::Continue => {}
                Flow::Break => break,
                Flow::Return(v) => {
                    st.seek(end)?;
                    return Ok(Flow::Return(v));
                }
            }
        }
        st.seek(end)?;
        Ok(Flow::Normal)
    }

    /// Shared `Field`/`IndexField` element-list walk. `make` builds the
    /// node object for each named unit from (access, bit offset, bits).
    fn field_list(
        &mut self,
        st: &mut Stream,
        end: usize,
        frame: &mut Frame,
        flags: u8,
        make: impl Fn(AccessType, u64, u32) -> NodeObject,
    ) -> Result<Flow, AmlError> {
        let mut access = AccessType::from_flags(flags);
        let mut bit_off: u64 = 0;
        while st.pos() < end {
            match st.peek().ok_or(AmlError::Truncated)? {
                0x00 => {
                    // ReservedField: skip pad bits.
                    st.next_u8()?;
                    let bits = st.pkg_length_value()? as u64;
                    bit_off = bit_off.checked_add(bits).ok_or(AmlError::Malformed)?;
                }
                0x01 => {
                    // AccessField.
                    st.next_u8()?;
                    let ty = st.next_u8()?;
                    let _attrib = st.next_u8()?;
                    access = AccessType::from_flags(ty);
                }
                0x02 => {
                    // ConnectField: a GPIO/I2C connection descriptor —
                    // parsed to stay in sync, semantics out of subset.
                    st.next_u8()?;
                    if st.peek() == Some(decode::BUFFER_OP) {
                        let _ = self.eval_term_arg(st, frame)?;
                    } else {
                        let _ = st.name_string()?;
                    }
                }
                0x03 => {
                    // ExtendedAccessField.
                    st.next_u8()?;
                    let ty = st.next_u8()?;
                    let _attrib = st.next_u8()?;
                    let _len = st.next_u8()?;
                    access = AccessType::from_flags(ty);
                }
                b if decode::is_lead_name_char(b) => {
                    let seg_bytes = st.take(4)?;
                    let mut seg = [0u8; 4];
                    seg.copy_from_slice(seg_bytes);
                    if !seg.iter().all(|&c| decode::is_name_char(c)) {
                        return Err(AmlError::BadNameString);
                    }
                    let bits = st.pkg_length_value()? as u64;
                    if bits == 0 || bits > u32::MAX as u64 {
                        return Err(AmlError::Malformed);
                    }
                    let object = make(access, bit_off, bits as u32);
                    self.ns.attach(frame.scope, seg, object);
                    bit_off = bit_off.checked_add(bits).ok_or(AmlError::Malformed)?;
                }
                _ => return Err(AmlError::Malformed),
            }
        }
        st.seek(end)?;
        Ok(Flow::Normal)
    }

    // -----------------------------------------------------------------
    // Expression evaluation
    // -----------------------------------------------------------------

    fn eval_int(&mut self, st: &mut Stream, frame: &mut Frame) -> Result<u64, AmlError> {
        self.eval_term_arg(st, frame)?.as_integer()
    }

    /// `DataRefObject` position (the value of a `Name`, a package
    /// element): a NameString here is an object *reference*, never a
    /// method invocation.
    fn eval_data_ref_object(
        &mut self,
        st: &mut Stream,
        frame: &mut Frame,
    ) -> Result<AmlValue, AmlError> {
        if st.at_name_string() {
            let path = st.name_string()?;
            return Ok(match self.ns.resolve(frame.scope, &path) {
                Some(n) => AmlValue::ObjectRef(n),
                None => AmlValue::NamePath(path.display()),
            });
        }
        self.eval_term_arg(st, frame)
    }

    fn eval_term_arg(&mut self, st: &mut Stream, frame: &mut Frame) -> Result<AmlValue, AmlError> {
        self.tick()?;
        self.guarded(|s| s.eval_term_arg_inner(st, frame))
    }

    fn eval_term_arg_inner(
        &mut self,
        st: &mut Stream,
        frame: &mut Frame,
    ) -> Result<AmlValue, AmlError> {
        if st.at_name_string() {
            return self.eval_name_expr(st, frame);
        }
        let op = st.next_u8()?;
        match op {
            decode::ZERO_OP => Ok(AmlValue::Integer(0)),
            decode::ONE_OP => Ok(AmlValue::Integer(1)),
            decode::ONES_OP => Ok(AmlValue::Integer(ONES)),
            decode::BYTE_PREFIX => Ok(AmlValue::Integer(st.next_u8()? as u64)),
            decode::WORD_PREFIX => Ok(AmlValue::Integer(st.next_u16()? as u64)),
            decode::DWORD_PREFIX => Ok(AmlValue::Integer(st.next_u32()? as u64)),
            decode::QWORD_PREFIX => Ok(AmlValue::Integer(st.next_u64()?)),
            decode::STRING_PREFIX => {
                let mut s = String::new();
                loop {
                    let b = st.next_u8()?;
                    if b == 0 {
                        break;
                    }
                    s.push(b as char);
                }
                Ok(AmlValue::String(s))
            }
            decode::BUFFER_OP => self.eval_buffer(st, frame),
            decode::PACKAGE_OP => self.eval_package(st, frame, false),
            decode::VAR_PACKAGE_OP => self.eval_package(st, frame, true),
            b @ decode::LOCAL0_OP..=decode::LOCAL7_OP => {
                Ok(frame.locals[(b - decode::LOCAL0_OP) as usize].clone())
            }
            b @ decode::ARG0_OP..=decode::ARG6_OP => Ok(frame
                .args
                .get((b - decode::ARG0_OP) as usize)
                .cloned()
                .unwrap_or(AmlValue::Uninitialized)),
            decode::STORE_OP => {
                let v = self.eval_term_arg(st, frame)?;
                let t = self.eval_target(st, frame)?;
                self.store_to(t, v.clone(), frame)?;
                Ok(v)
            }
            decode::COPY_OBJECT_OP => {
                let v = self.eval_term_arg(st, frame)?;
                let t = self.eval_target(st, frame)?;
                self.store_to(t, v.clone(), frame)?;
                Ok(v)
            }
            decode::ADD_OP => self.binary_int_op(st, frame, |a, b| Ok(a.wrapping_add(b))),
            decode::SUBTRACT_OP => self.binary_int_op(st, frame, |a, b| Ok(a.wrapping_sub(b))),
            decode::MULTIPLY_OP => self.binary_int_op(st, frame, |a, b| Ok(a.wrapping_mul(b))),
            decode::SHIFT_LEFT_OP => {
                self.binary_int_op(st, frame, |a, b| Ok(if b >= 64 { 0 } else { a << b }))
            }
            decode::SHIFT_RIGHT_OP => {
                self.binary_int_op(st, frame, |a, b| Ok(if b >= 64 { 0 } else { a >> b }))
            }
            decode::AND_OP => self.binary_int_op(st, frame, |a, b| Ok(a & b)),
            decode::OR_OP => self.binary_int_op(st, frame, |a, b| Ok(a | b)),
            decode::NAND_OP => self.binary_int_op(st, frame, |a, b| Ok(!(a & b))),
            decode::NOR_OP => self.binary_int_op(st, frame, |a, b| Ok(!(a | b))),
            decode::XOR_OP => self.binary_int_op(st, frame, |a, b| Ok(a ^ b)),
            decode::MOD_OP => self.binary_int_op(st, frame, |a, b| {
                if b == 0 {
                    Err(AmlError::DivideByZero)
                } else {
                    Ok(a % b)
                }
            }),
            decode::DIVIDE_OP => {
                let dividend = self.eval_int(st, frame)?;
                let divisor = self.eval_int(st, frame)?;
                if divisor == 0 {
                    return Err(AmlError::DivideByZero);
                }
                let rem_t = self.eval_target(st, frame)?;
                let quot_t = self.eval_target(st, frame)?;
                self.store_to(rem_t, AmlValue::Integer(dividend % divisor), frame)?;
                let q = AmlValue::Integer(dividend / divisor);
                self.store_to(quot_t, q.clone(), frame)?;
                Ok(q)
            }
            decode::NOT_OP => {
                let v = self.eval_int(st, frame)?;
                let t = self.eval_target(st, frame)?;
                let r = AmlValue::Integer(!v);
                self.store_to(t, r.clone(), frame)?;
                Ok(r)
            }
            decode::FIND_SET_LEFT_BIT_OP => {
                let v = self.eval_int(st, frame)?;
                let t = self.eval_target(st, frame)?;
                let r = AmlValue::Integer(if v == 0 {
                    0
                } else {
                    64 - v.leading_zeros() as u64
                });
                self.store_to(t, r.clone(), frame)?;
                Ok(r)
            }
            decode::FIND_SET_RIGHT_BIT_OP => {
                let v = self.eval_int(st, frame)?;
                let t = self.eval_target(st, frame)?;
                let r = AmlValue::Integer(if v == 0 {
                    0
                } else {
                    v.trailing_zeros() as u64 + 1
                });
                self.store_to(t, r.clone(), frame)?;
                Ok(r)
            }
            decode::INCREMENT_OP | decode::DECREMENT_OP => {
                let t = self.eval_target(st, frame)?;
                let cur = self.target_read(&t, frame)?.as_integer()?;
                let next = if op == decode::INCREMENT_OP {
                    cur.wrapping_add(1)
                } else {
                    cur.wrapping_sub(1)
                };
                self.store_to(t, AmlValue::Integer(next), frame)?;
                Ok(AmlValue::Integer(next))
            }
            decode::LAND_OP => {
                let a = self.eval_int(st, frame)?;
                let b = self.eval_int(st, frame)?;
                Ok(bool_val(a != 0 && b != 0))
            }
            decode::LOR_OP => {
                let a = self.eval_int(st, frame)?;
                let b = self.eval_int(st, frame)?;
                Ok(bool_val(a != 0 || b != 0))
            }
            decode::LNOT_OP => {
                let a = self.eval_int(st, frame)?;
                Ok(bool_val(a == 0))
            }
            decode::LEQUAL_OP => {
                let ord = self.compare(st, frame)?;
                Ok(bool_val(ord == core::cmp::Ordering::Equal))
            }
            decode::LGREATER_OP => {
                let ord = self.compare(st, frame)?;
                Ok(bool_val(ord == core::cmp::Ordering::Greater))
            }
            decode::LLESS_OP => {
                let ord = self.compare(st, frame)?;
                Ok(bool_val(ord == core::cmp::Ordering::Less))
            }
            decode::CONCAT_OP => self.eval_concat(st, frame),
            decode::CONCAT_RES_OP => self.eval_concat_res(st, frame),
            decode::SIZE_OF_OP => {
                let t = self.eval_target(st, frame)?;
                let v = self.target_read(&t, frame)?;
                let n = match &v {
                    AmlValue::Buffer(b) => b.len() as u64,
                    AmlValue::String(s) => s.len() as u64,
                    AmlValue::Package(p) => p.len() as u64,
                    _ => return Err(AmlError::TypeMismatch),
                };
                Ok(AmlValue::Integer(n))
            }
            decode::INDEX_OP => {
                let src = self.eval_term_arg(st, frame)?;
                let idx = self.eval_int(st, frame)?;
                let t = self.eval_target(st, frame)?;
                let elem = index_value(&self.resolve_ref(src)?, idx)?;
                self.store_to(t, elem.clone(), frame)?;
                Ok(elem)
            }
            decode::DEREF_OF_OP => {
                let v = self.eval_term_arg(st, frame)?;
                self.resolve_ref(v)
            }
            decode::REF_OF_OP => {
                let t = self.eval_target(st, frame)?;
                match t {
                    Target::Node(n) => Ok(AmlValue::ObjectRef(n)),
                    _ => Err(AmlError::TypeMismatch),
                }
            }
            decode::OBJECT_TYPE_OP => {
                let t = self.eval_target(st, frame)?;
                let code = match &t {
                    Target::Node(n) => node_type_code(&self.ns.node(*n).object),
                    Target::Local(i) => frame.locals[*i as usize].object_type(),
                    Target::Arg(i) => frame
                        .args
                        .get(*i as usize)
                        .map(|v| v.object_type())
                        .unwrap_or(0),
                    _ => 0,
                };
                Ok(AmlValue::Integer(code))
            }
            decode::TO_BUFFER_OP => {
                let v = self.eval_term_arg(st, frame)?;
                let t = self.eval_target(st, frame)?;
                let r = AmlValue::Buffer(to_buffer_bytes(&v)?);
                self.store_to(t, r.clone(), frame)?;
                Ok(r)
            }
            decode::TO_INTEGER_OP => {
                let v = self.eval_term_arg(st, frame)?;
                let t = self.eval_target(st, frame)?;
                let n = match &v {
                    AmlValue::String(s) => parse_explicit_integer(s)?,
                    other => other.as_integer()?,
                };
                let r = AmlValue::Integer(n);
                self.store_to(t, r.clone(), frame)?;
                Ok(r)
            }
            decode::TO_HEX_STRING_OP => {
                let v = self.eval_term_arg(st, frame)?;
                let t = self.eval_target(st, frame)?;
                let r = AmlValue::String(to_hex_string(&v)?);
                self.store_to(t, r.clone(), frame)?;
                Ok(r)
            }
            decode::TO_DECIMAL_STRING_OP => {
                let v = self.eval_term_arg(st, frame)?;
                let t = self.eval_target(st, frame)?;
                let r = AmlValue::String(to_decimal_string(&v)?);
                self.store_to(t, r.clone(), frame)?;
                Ok(r)
            }
            decode::TO_STRING_OP => {
                let v = self.eval_term_arg(st, frame)?;
                let len = self.eval_int(st, frame)?;
                let t = self.eval_target(st, frame)?;
                let AmlValue::Buffer(b) = v else {
                    return Err(AmlError::TypeMismatch);
                };
                let take = if len == ONES {
                    b.len()
                } else {
                    (len as usize).min(b.len())
                };
                let mut s = String::new();
                for &c in b[..take].iter().take_while(|&&c| c != 0) {
                    s.push(c as char);
                }
                let r = AmlValue::String(s);
                self.store_to(t, r.clone(), frame)?;
                Ok(r)
            }
            decode::MID_OP => {
                let v = self.eval_term_arg(st, frame)?;
                let idx = self.eval_int(st, frame)? as usize;
                let len = self.eval_int(st, frame)? as usize;
                let t = self.eval_target(st, frame)?;
                let r = match &v {
                    AmlValue::String(s) => {
                        let b = s.as_bytes();
                        let start = idx.min(b.len());
                        let end = start.saturating_add(len).min(b.len());
                        AmlValue::String(b[start..end].iter().map(|&c| c as char).collect())
                    }
                    AmlValue::Buffer(b) => {
                        let start = idx.min(b.len());
                        let end = start.saturating_add(len).min(b.len());
                        AmlValue::Buffer(b[start..end].to_vec())
                    }
                    _ => return Err(AmlError::TypeMismatch),
                };
                self.store_to(t, r.clone(), frame)?;
                Ok(r)
            }
            decode::MATCH_OP => self.eval_match(st, frame),
            decode::NOTIFY_OP => {
                let t = self.eval_target(st, frame)?;
                let code = self.eval_int(st, frame)?;
                if let Target::Node(n) = t {
                    self.ns.pending_notify.push((n, code));
                }
                Ok(AmlValue::Integer(0))
            }
            decode::CREATE_BIT_FIELD_OP => self.create_buffer_field(st, frame, Some(1), true),
            decode::CREATE_BYTE_FIELD_OP => self.create_buffer_field(st, frame, Some(8), false),
            decode::CREATE_WORD_FIELD_OP => self.create_buffer_field(st, frame, Some(16), false),
            decode::CREATE_DWORD_FIELD_OP => self.create_buffer_field(st, frame, Some(32), false),
            decode::CREATE_QWORD_FIELD_OP => self.create_buffer_field(st, frame, Some(64), false),
            decode::EXT_OP_PREFIX => {
                let ext = st.next_u8()?;
                self.eval_ext_expr(st, frame, ext)
            }
            other => Err(AmlError::UnsupportedOpcode(other as u16)),
        }
    }

    fn eval_ext_expr(
        &mut self,
        st: &mut Stream,
        frame: &mut Frame,
        ext: u8,
    ) -> Result<AmlValue, AmlError> {
        match ext {
            decode::EXT_REVISION_OP => Ok(AmlValue::Integer(2)),
            decode::EXT_TIMER_OP => {
                // Monotonic-ish 100 ns ticks; derived from the op budget
                // so `While (Timer < deadline)` loops make progress.
                Ok(AmlValue::Integer(self.ops.wrapping_mul(10_000)))
            }
            decode::EXT_COND_REF_OF_OP => {
                let found = match self.eval_target(st, frame) {
                    Ok(Target::Node(n)) => Some(n),
                    Ok(_) => None,
                    Err(AmlError::UnresolvedName) => None,
                    Err(e) => return Err(e),
                };
                let t = self.eval_target(st, frame)?;
                match found {
                    Some(n) => {
                        self.store_to(t, AmlValue::ObjectRef(n), frame)?;
                        Ok(AmlValue::Integer(ONES))
                    }
                    None => Ok(AmlValue::Integer(0)),
                }
            }
            decode::EXT_CREATE_FIELD_OP => {
                // CreateField(source, bit index, NUM BITS, name).
                self.create_buffer_field(st, frame, None, true)
            }
            decode::EXT_SLEEP_OP => {
                let ms = self.eval_int(st, frame)?;
                self.regions.sleep_ms(ms);
                Ok(AmlValue::Integer(0))
            }
            decode::EXT_STALL_OP => {
                let us = self.eval_int(st, frame)?;
                self.regions.sleep_ms(us.div_ceil(1000));
                Ok(AmlValue::Integer(0))
            }
            decode::EXT_ACQUIRE_OP => {
                let _mutex = self.eval_target(st, frame)?;
                let _timeout = st.next_u16()?;
                // Single-threaded interpreter: always acquired (Zero =
                // success per §19.6.2).
                Ok(AmlValue::Integer(0))
            }
            decode::EXT_RELEASE_OP | decode::EXT_SIGNAL_OP | decode::EXT_RESET_OP => {
                let _obj = self.eval_target(st, frame)?;
                Ok(AmlValue::Integer(0))
            }
            decode::EXT_WAIT_OP => {
                let _event = self.eval_target(st, frame)?;
                let _timeout = self.eval_int(st, frame)?;
                // Zero = signaled (no event sources in the subset).
                Ok(AmlValue::Integer(0))
            }
            decode::EXT_FROM_BCD_OP => {
                let v = self.eval_int(st, frame)?;
                let t = self.eval_target(st, frame)?;
                let mut out: u64 = 0;
                let mut mul: u64 = 1;
                let mut x = v;
                while x != 0 {
                    out = out.wrapping_add((x & 0xF) * mul);
                    mul = mul.wrapping_mul(10);
                    x >>= 4;
                }
                let r = AmlValue::Integer(out);
                self.store_to(t, r.clone(), frame)?;
                Ok(r)
            }
            decode::EXT_TO_BCD_OP => {
                let v = self.eval_int(st, frame)?;
                let t = self.eval_target(st, frame)?;
                let mut out: u64 = 0;
                let mut shift = 0;
                let mut x = v;
                while x != 0 && shift < 64 {
                    out |= (x % 10) << shift;
                    x /= 10;
                    shift += 4;
                }
                let r = AmlValue::Integer(out);
                self.store_to(t, r.clone(), frame)?;
                Ok(r)
            }
            decode::EXT_FATAL_OP => {
                let _type = st.next_u8()?;
                let _code = st.next_u32()?;
                let _arg = self.eval_term_arg(st, frame)?;
                Err(AmlError::Malformed)
            }
            decode::EXT_DEBUG_OP => {
                // Debug object in expression position reads as zero.
                Ok(AmlValue::Integer(0))
            }
            other => Err(AmlError::UnsupportedOpcode(0x8000 | other as u16)),
        }
    }

    /// NameString in expression position: a method invocation (args
    /// follow inline) or a named-object read.
    fn eval_name_expr(&mut self, st: &mut Stream, frame: &mut Frame) -> Result<AmlValue, AmlError> {
        let path = st.name_string()?;
        let node = self
            .ns
            .resolve(frame.scope, &path)
            .ok_or(AmlError::UnresolvedName)?;
        match self.ns.node(node).object {
            NodeObject::Method { arg_count, .. } => {
                let mut args = Vec::new();
                for _ in 0..arg_count {
                    args.push(self.eval_term_arg(st, frame)?);
                }
                self.invoke_method(node, args)
            }
            NodeObject::ExternalMethod { arg_count } => {
                for _ in 0..arg_count {
                    let _ = self.eval_term_arg(st, frame)?;
                }
                Ok(AmlValue::Integer(0))
            }
            _ => self.read_node(node),
        }
    }

    pub fn invoke_method(
        &mut self,
        node: NodeId,
        args: Vec<AmlValue>,
    ) -> Result<AmlValue, AmlError> {
        let NodeObject::Method {
            table,
            start,
            end,
            arg_count,
            ..
        } = self.ns.node(node).object
        else {
            return Err(AmlError::TypeMismatch);
        };
        if table == u32::MAX {
            return Ok(builtin_osi(args.first()));
        }
        let bytes = self
            .ns
            .tables
            .get(table as usize)
            .cloned()
            .ok_or(AmlError::Malformed)?;
        let mut frame = Frame::new(node);
        frame.args = args;
        frame
            .args
            .resize(arg_count as usize, AmlValue::Uninitialized);
        let prev_table = self.current_table;
        let prev_load = self.in_load;
        self.current_table = table;
        self.in_load = false;
        let mut st = Stream::new(&bytes);
        let result = st.seek(start as usize).and_then(|_| {
            self.guarded(|s| s.exec_term_list(&mut st, end as usize, &mut frame, false))
        });
        self.current_table = prev_table;
        self.in_load = prev_load;
        match result? {
            Flow::Return(v) => Ok(v),
            // No explicit Return: yield Zero (the ACPICA "implicit
            // return" compatibility posture firmware tends to assume).
            _ => Ok(AmlValue::Integer(0)),
        }
    }

    // -----------------------------------------------------------------
    // Operator helpers
    // -----------------------------------------------------------------

    fn binary_int_op(
        &mut self,
        st: &mut Stream,
        frame: &mut Frame,
        f: impl Fn(u64, u64) -> Result<u64, AmlError>,
    ) -> Result<AmlValue, AmlError> {
        let a = self.eval_int(st, frame)?;
        let b = self.eval_int(st, frame)?;
        let t = self.eval_target(st, frame)?;
        let r = AmlValue::Integer(f(a, b)?);
        self.store_to(t, r.clone(), frame)?;
        Ok(r)
    }

    fn compare(
        &mut self,
        st: &mut Stream,
        frame: &mut Frame,
    ) -> Result<core::cmp::Ordering, AmlError> {
        let a = self.eval_term_arg(st, frame)?;
        let b = self.eval_term_arg(st, frame)?;
        match &a {
            AmlValue::Integer(_) => Ok(a.as_integer()?.cmp(&b.as_integer()?)),
            AmlValue::String(x) => match &b {
                AmlValue::String(y) => Ok(x.as_bytes().cmp(y.as_bytes())),
                _ => Err(AmlError::TypeMismatch),
            },
            AmlValue::Buffer(x) => match &b {
                AmlValue::Buffer(y) => Ok(x.as_slice().cmp(y.as_slice())),
                AmlValue::Integer(_) => Ok(a.as_integer()?.cmp(&b.as_integer()?)),
                _ => Err(AmlError::TypeMismatch),
            },
            _ => Err(AmlError::TypeMismatch),
        }
    }

    fn eval_buffer(&mut self, st: &mut Stream, frame: &mut Frame) -> Result<AmlValue, AmlError> {
        let end = st.pkg_end()?;
        let size = self.eval_int(st, frame)? as usize;
        if size > MAX_BUFFER {
            return Err(AmlError::Malformed);
        }
        let raw_len = end.checked_sub(st.pos()).ok_or(AmlError::Malformed)?;
        let raw = st.take(raw_len)?;
        let mut out = alloc::vec![0u8; size.max(raw.len())];
        out[..raw.len()].copy_from_slice(raw);
        Ok(AmlValue::Buffer(out))
    }

    fn eval_package(
        &mut self,
        st: &mut Stream,
        frame: &mut Frame,
        var: bool,
    ) -> Result<AmlValue, AmlError> {
        let end = st.pkg_end()?;
        let declared = if var {
            self.eval_int(st, frame)? as usize
        } else {
            st.next_u8()? as usize
        };
        if declared > MAX_PACKAGE {
            return Err(AmlError::Malformed);
        }
        let mut elems: Vec<AmlValue> = Vec::new();
        while st.pos() < end {
            self.tick()?;
            let v = self.eval_data_ref_object(st, frame)?;
            elems.push(v);
            if elems.len() > MAX_PACKAGE {
                return Err(AmlError::Malformed);
            }
        }
        st.seek(end)?;
        while elems.len() < declared {
            elems.push(AmlValue::Uninitialized);
        }
        Ok(AmlValue::Package(elems))
    }

    fn eval_concat(&mut self, st: &mut Stream, frame: &mut Frame) -> Result<AmlValue, AmlError> {
        let a = self.eval_term_arg(st, frame)?;
        let b = self.eval_term_arg(st, frame)?;
        let t = self.eval_target(st, frame)?;
        let r = match &a {
            AmlValue::Integer(x) => {
                let y = b.as_integer()?;
                let mut out = Vec::with_capacity(16);
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
                AmlValue::Buffer(out)
            }
            AmlValue::String(x) => {
                let mut s = x.clone();
                match &b {
                    AmlValue::String(y) => s.push_str(y),
                    AmlValue::Integer(_) => s.push_str(&to_hex_string(&b)?),
                    AmlValue::Buffer(_) => s.push_str(&to_hex_string(&b)?),
                    _ => return Err(AmlError::TypeMismatch),
                }
                AmlValue::String(s)
            }
            AmlValue::Buffer(x) => {
                let mut out = x.clone();
                out.extend_from_slice(&to_buffer_bytes(&b)?);
                AmlValue::Buffer(out)
            }
            _ => return Err(AmlError::TypeMismatch),
        };
        self.store_to(t, r.clone(), frame)?;
        Ok(r)
    }

    fn eval_concat_res(
        &mut self,
        st: &mut Stream,
        frame: &mut Frame,
    ) -> Result<AmlValue, AmlError> {
        let a = self.eval_term_arg(st, frame)?;
        let b = self.eval_term_arg(st, frame)?;
        let t = self.eval_target(st, frame)?;
        let (AmlValue::Buffer(x), AmlValue::Buffer(y)) = (&a, &b) else {
            return Err(AmlError::TypeMismatch);
        };
        let strip = |buf: &[u8]| -> Vec<u8> {
            // Drop a trailing EndTag (0x79 checksum) if present.
            if buf.len() >= 2 && buf[buf.len() - 2] == 0x79 {
                buf[..buf.len() - 2].to_vec()
            } else {
                buf.to_vec()
            }
        };
        let mut out = strip(x);
        out.extend_from_slice(&strip(y));
        out.extend_from_slice(&[0x79, 0x00]);
        let r = AmlValue::Buffer(out);
        self.store_to(t, r.clone(), frame)?;
        Ok(r)
    }

    fn eval_match(&mut self, st: &mut Stream, frame: &mut Frame) -> Result<AmlValue, AmlError> {
        let pkg = self.eval_term_arg(st, frame)?;
        let op1 = st.next_u8()?;
        let v1 = self.eval_int(st, frame)?;
        let op2 = st.next_u8()?;
        let v2 = self.eval_int(st, frame)?;
        let start = self.eval_int(st, frame)? as usize;
        let AmlValue::Package(elems) = pkg else {
            return Err(AmlError::TypeMismatch);
        };
        let check = |mop: u8, elem: u64, operand: u64| -> Result<bool, AmlError> {
            Ok(match mop {
                0 => true,            // MTR
                1 => elem == operand, // MEQ
                2 => elem <= operand, // MLE
                3 => elem < operand,  // MLT
                4 => elem >= operand, // MGE
                5 => elem > operand,  // MGT
                _ => return Err(AmlError::Malformed),
            })
        };
        for (i, e) in elems.iter().enumerate().skip(start) {
            let Ok(ev) = e.as_integer() else { continue };
            if check(op1, ev, v1)? && check(op2, ev, v2)? {
                return Ok(AmlValue::Integer(i as u64));
            }
        }
        Ok(AmlValue::Integer(ONES))
    }

    /// `CreateBitField`/`CreateByteField`/…/`CreateField` over a *named*
    /// buffer. `width`: fixed bit width, or `None` for `CreateField`
    /// (explicit bit-count operand). `bit_index`: whether the index
    /// operand counts bits (true) or bytes.
    fn create_buffer_field(
        &mut self,
        st: &mut Stream,
        frame: &mut Frame,
        width: Option<u32>,
        bit_index: bool,
    ) -> Result<AmlValue, AmlError> {
        if !st.at_name_string() {
            // Buffer fields over expression/local sources are outside the
            // enumeration subset.
            return Err(AmlError::UnsupportedOpcode(0x8A));
        }
        let src_path = st.name_string()?;
        let buffer = self
            .ns
            .resolve(frame.scope, &src_path)
            .ok_or(AmlError::UnresolvedName)?;
        let idx = self.eval_int(st, frame)?;
        let bits = match width {
            Some(w) => w,
            None => {
                let n = self.eval_int(st, frame)?;
                if n == 0 || n > u32::MAX as u64 {
                    return Err(AmlError::Malformed);
                }
                n as u32
            }
        };
        let name = st.name_string()?;
        let bit_offset = if bit_index {
            idx
        } else {
            idx.checked_mul(8).ok_or(AmlError::Malformed)?
        };
        self.ns.create_path(
            frame.scope,
            &name,
            NodeObject::BufferField(BufferField {
                buffer,
                bit_offset,
                bit_len: bits,
            }),
        )?;
        Ok(AmlValue::Integer(0))
    }

    // -----------------------------------------------------------------
    // Targets, stores, reads
    // -----------------------------------------------------------------

    fn eval_target(&mut self, st: &mut Stream, frame: &mut Frame) -> Result<Target, AmlError> {
        match st.peek().ok_or(AmlError::Truncated)? {
            0x00 => {
                st.next_u8()?;
                Ok(Target::Null)
            }
            b @ decode::LOCAL0_OP..=decode::LOCAL7_OP => {
                st.next_u8()?;
                Ok(Target::Local(b - decode::LOCAL0_OP))
            }
            b @ decode::ARG0_OP..=decode::ARG6_OP => {
                st.next_u8()?;
                Ok(Target::Arg(b - decode::ARG0_OP))
            }
            decode::EXT_OP_PREFIX if st.peek_at(1) == Some(decode::EXT_DEBUG_OP) => {
                st.take(2)?;
                Ok(Target::Debug)
            }
            decode::INDEX_OP => {
                st.next_u8()?;
                let base = self.eval_target(st, frame)?;
                let idx = self.eval_int(st, frame)?;
                // DefIndex carries its own (usually Null) Target operand.
                let inner = self.eval_target(st, frame)?;
                let t = Target::Index(alloc::boxed::Box::new(base), idx);
                if !matches!(inner, Target::Null) {
                    let v = self.target_read(&t, frame)?;
                    self.store_to(inner, v, frame)?;
                }
                Ok(t)
            }
            _ if st.at_name_string() => {
                let path = st.name_string()?;
                let node = self
                    .ns
                    .resolve(frame.scope, &path)
                    .ok_or(AmlError::UnresolvedName)?;
                Ok(Target::Node(node))
            }
            other => Err(AmlError::UnsupportedOpcode(other as u16)),
        }
    }

    fn target_read(&mut self, t: &Target, frame: &mut Frame) -> Result<AmlValue, AmlError> {
        match t {
            Target::Null | Target::Debug => Err(AmlError::TypeMismatch),
            Target::Local(i) => Ok(frame.locals[*i as usize].clone()),
            Target::Arg(i) => Ok(frame
                .args
                .get(*i as usize)
                .cloned()
                .unwrap_or(AmlValue::Uninitialized)),
            Target::Node(n) => self.read_node(*n),
            Target::Index(base, idx) => {
                let container = self.target_read(base, frame)?;
                index_value(&container, *idx)
            }
        }
    }

    fn store_to(&mut self, t: Target, value: AmlValue, frame: &mut Frame) -> Result<(), AmlError> {
        match t {
            Target::Null | Target::Debug => Ok(()),
            Target::Local(i) => {
                frame.locals[i as usize] = value;
                Ok(())
            }
            Target::Arg(i) => {
                let i = i as usize;
                if frame.args.len() <= i {
                    frame.args.resize(i + 1, AmlValue::Uninitialized);
                }
                frame.args[i] = value;
                Ok(())
            }
            Target::Node(n) => self.store_node(n, value),
            Target::Index(base, idx) => {
                let slot = Self::target_slot(self.ns, &base, frame)?;
                match slot {
                    AmlValue::Buffer(b) => {
                        let i = idx as usize;
                        if i >= b.len() {
                            return Err(AmlError::IndexOutOfRange);
                        }
                        b[i] = value.as_integer()? as u8;
                        Ok(())
                    }
                    AmlValue::String(s) => {
                        let i = idx as usize;
                        let mut bytes: Vec<u8> = s.bytes().collect();
                        if i >= bytes.len() {
                            return Err(AmlError::IndexOutOfRange);
                        }
                        bytes[i] = value.as_integer()? as u8;
                        *s = bytes.iter().map(|&c| c as char).collect();
                        Ok(())
                    }
                    AmlValue::Package(p) => {
                        let i = idx as usize;
                        if i >= p.len() {
                            return Err(AmlError::IndexOutOfRange);
                        }
                        p[i] = value;
                        Ok(())
                    }
                    _ => Err(AmlError::TypeMismatch),
                }
            }
        }
    }

    /// Mutable slot behind a target chain (for `Index` stores). Only
    /// value-bearing bases are addressable: locals, args, `Name` nodes,
    /// and package elements thereof.
    fn target_slot<'f>(
        ns: &'f mut Namespace,
        t: &Target,
        frame: &'f mut Frame,
    ) -> Result<&'f mut AmlValue, AmlError> {
        match t {
            Target::Local(i) => Ok(&mut frame.locals[*i as usize]),
            Target::Arg(i) => frame.args.get_mut(*i as usize).ok_or(AmlError::BadArg),
            Target::Node(n) => {
                let n = ns.deref_alias(*n);
                match &mut ns.node_mut(n).object {
                    NodeObject::Name(v) => Ok(v),
                    _ => Err(AmlError::TypeMismatch),
                }
            }
            Target::Index(base, idx) => {
                let slot = Self::target_slot(ns, base, frame)?;
                match slot {
                    AmlValue::Package(p) => {
                        p.get_mut(*idx as usize).ok_or(AmlError::IndexOutOfRange)
                    }
                    _ => Err(AmlError::TypeMismatch),
                }
            }
            _ => Err(AmlError::TypeMismatch),
        }
    }

    fn store_node(&mut self, node: NodeId, value: AmlValue) -> Result<(), AmlError> {
        let node = self.ns.deref_alias(node);
        match self.ns.node(node).object.clone() {
            NodeObject::Name(_) => {
                self.ns.node_mut(node).object = NodeObject::Name(value);
                Ok(())
            }
            NodeObject::Field(f) => self.write_field(&f, &value),
            NodeObject::IndexField(f) => self.write_index_field(&f, &value),
            NodeObject::BufferField(f) => self.write_buffer_field(&f, &value),
            NodeObject::External => Ok(()),
            _ => Err(AmlError::TypeMismatch),
        }
    }

    fn read_node(&mut self, node: NodeId) -> Result<AmlValue, AmlError> {
        let node = self.ns.deref_alias(node);
        match self.ns.node(node).object.clone() {
            NodeObject::Name(v) => Ok(v),
            NodeObject::Field(f) => self.read_field(&f),
            NodeObject::IndexField(f) => self.read_index_field(&f),
            NodeObject::BufferField(f) => self.read_buffer_field(&f),
            NodeObject::Method { arg_count: 0, .. } => self.invoke_method(node, Vec::new()),
            NodeObject::External => Ok(AmlValue::Integer(0)),
            // Devices, scopes, regions, mutexes, … read as a reference.
            _ => Ok(AmlValue::ObjectRef(node)),
        }
    }

    // -----------------------------------------------------------------
    // Field access
    // -----------------------------------------------------------------

    fn region_params(&self, region: NodeId) -> Result<(u8, u64, u64), AmlError> {
        match self.ns.node(self.ns.deref_alias(region)).object {
            NodeObject::OpRegion {
                space,
                offset,
                length,
            } => Ok((space, offset, length)),
            _ => Err(AmlError::TypeMismatch),
        }
    }

    fn read_field(&mut self, f: &FieldUnit) -> Result<AmlValue, AmlError> {
        if f.bit_len == 0 {
            return Err(AmlError::Malformed);
        }
        let (space, base, region_len) = self.region_params(f.region)?;
        let last_bit = f
            .bit_offset
            .checked_add(f.bit_len as u64 - 1)
            .ok_or(AmlError::Malformed)?;
        if last_bit / 8 >= region_len {
            return Err(AmlError::RegionAccess);
        }
        let width = f.access.bits() as u64;
        let mut bits: Vec<u8> = alloc::vec![0; (f.bit_len as usize).div_ceil(8)];
        let mut out_pos: usize = 0;
        let mut chunk = (f.bit_offset / width) * width;
        while chunk <= last_bit {
            let v = self.regions.read(space, base + chunk / 8, width as u32)?;
            let lo = f.bit_offset.max(chunk);
            let hi = last_bit.min(chunk + width - 1);
            for bit in lo..=hi {
                if (v >> (bit - chunk)) & 1 != 0 {
                    bits[out_pos / 8] |= 1 << (out_pos % 8);
                }
                out_pos += 1;
            }
            chunk += width;
        }
        Ok(bits_to_value(bits, f.bit_len))
    }

    fn write_field(&mut self, f: &FieldUnit, value: &AmlValue) -> Result<(), AmlError> {
        if f.bit_len == 0 {
            return Err(AmlError::Malformed);
        }
        let (space, base, region_len) = self.region_params(f.region)?;
        let last_bit = f
            .bit_offset
            .checked_add(f.bit_len as u64 - 1)
            .ok_or(AmlError::Malformed)?;
        if last_bit / 8 >= region_len {
            return Err(AmlError::RegionAccess);
        }
        let src = to_buffer_bytes(value)?;
        let width = f.access.bits() as u64;
        let mut in_pos: usize = 0;
        let mut chunk = (f.bit_offset / width) * width;
        while chunk <= last_bit {
            // Read-modify-write (Preserve update rule; WriteAsOnes/Zeros
            // degrade to Preserve in this subset).
            let mut v = self.regions.read(space, base + chunk / 8, width as u32)?;
            let lo = f.bit_offset.max(chunk);
            let hi = last_bit.min(chunk + width - 1);
            for bit in lo..=hi {
                let sb = src
                    .get(in_pos / 8)
                    .map(|b| (b >> (in_pos % 8)) & 1)
                    .unwrap_or(0);
                if sb != 0 {
                    v |= 1 << (bit - chunk);
                } else {
                    v &= !(1 << (bit - chunk));
                }
                in_pos += 1;
            }
            self.regions
                .write(space, base + chunk / 8, width as u32, v)?;
            chunk += width;
        }
        Ok(())
    }

    fn read_index_field(&mut self, f: &IndexFieldUnit) -> Result<AmlValue, AmlError> {
        let (index_unit, data_unit) = self.index_field_units(f)?;
        self.write_field(&index_unit, &AmlValue::Integer(f.bit_offset / 8))?;
        let data = self.read_field(&data_unit)?.as_integer()?;
        let shift = (f.bit_offset % 8) as u32;
        let mask = if f.bit_len >= 64 {
            ONES
        } else {
            (1u64 << f.bit_len) - 1
        };
        Ok(AmlValue::Integer((data >> shift) & mask))
    }

    fn write_index_field(&mut self, f: &IndexFieldUnit, value: &AmlValue) -> Result<(), AmlError> {
        let (index_unit, data_unit) = self.index_field_units(f)?;
        self.write_field(&index_unit, &AmlValue::Integer(f.bit_offset / 8))?;
        let shift = (f.bit_offset % 8) as u32;
        let mask = if f.bit_len >= 64 {
            ONES
        } else {
            (1u64 << f.bit_len) - 1
        };
        let cur = self.read_field(&data_unit)?.as_integer()?;
        let v = (cur & !(mask << shift)) | ((value.as_integer()? & mask) << shift);
        self.write_field(&data_unit, &AmlValue::Integer(v))
    }

    fn index_field_units(&self, f: &IndexFieldUnit) -> Result<(FieldUnit, FieldUnit), AmlError> {
        let get = |n: NodeId| -> Result<FieldUnit, AmlError> {
            match &self.ns.node(self.ns.deref_alias(n)).object {
                NodeObject::Field(u) => Ok(u.clone()),
                _ => Err(AmlError::TypeMismatch),
            }
        };
        Ok((get(f.index)?, get(f.data)?))
    }

    fn buffer_field_bytes(&mut self, f: &BufferField) -> Result<Vec<u8>, AmlError> {
        let node = self.ns.deref_alias(f.buffer);
        match &self.ns.node(node).object {
            NodeObject::Name(AmlValue::Buffer(b)) => Ok(b.clone()),
            _ => Err(AmlError::TypeMismatch),
        }
    }

    fn read_buffer_field(&mut self, f: &BufferField) -> Result<AmlValue, AmlError> {
        if f.bit_len == 0 {
            return Err(AmlError::Malformed);
        }
        let src = self.buffer_field_bytes(f)?;
        let last_bit = f
            .bit_offset
            .checked_add(f.bit_len as u64 - 1)
            .ok_or(AmlError::Malformed)?;
        if last_bit / 8 >= src.len() as u64 {
            return Err(AmlError::IndexOutOfRange);
        }
        let mut bits: Vec<u8> = alloc::vec![0; (f.bit_len as usize).div_ceil(8)];
        for i in 0..f.bit_len as u64 {
            let bit = f.bit_offset + i;
            if (src[(bit / 8) as usize] >> (bit % 8)) & 1 != 0 {
                bits[(i / 8) as usize] |= 1 << (i % 8);
            }
        }
        Ok(bits_to_value(bits, f.bit_len))
    }

    fn write_buffer_field(&mut self, f: &BufferField, value: &AmlValue) -> Result<(), AmlError> {
        if f.bit_len == 0 {
            return Err(AmlError::Malformed);
        }
        let mut dst = self.buffer_field_bytes(f)?;
        let last_bit = f
            .bit_offset
            .checked_add(f.bit_len as u64 - 1)
            .ok_or(AmlError::Malformed)?;
        if last_bit / 8 >= dst.len() as u64 {
            return Err(AmlError::IndexOutOfRange);
        }
        let src = to_buffer_bytes(value)?;
        for i in 0..f.bit_len as u64 {
            let sb = src
                .get((i / 8) as usize)
                .map(|b| (b >> (i % 8)) & 1)
                .unwrap_or(0);
            let bit = f.bit_offset + i;
            let byte = &mut dst[(bit / 8) as usize];
            if sb != 0 {
                *byte |= 1 << (bit % 8);
            } else {
                *byte &= !(1 << (bit % 8));
            }
        }
        let node = self.ns.deref_alias(f.buffer);
        self.ns.node_mut(node).object = NodeObject::Name(AmlValue::Buffer(dst));
        Ok(())
    }

    /// Resolve reference-like values to the referenced data (`DerefOf`
    /// and `Index` source handling). Non-references pass through.
    fn resolve_ref(&mut self, v: AmlValue) -> Result<AmlValue, AmlError> {
        match v {
            AmlValue::ObjectRef(n) => self.read_node(n),
            AmlValue::NamePath(p) => {
                let node = self.ns.resolve_str(&p).ok_or(AmlError::UnresolvedName)?;
                self.read_node(node)
            }
            other => Ok(other),
        }
    }
}

// ---------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------

fn bool_val(b: bool) -> AmlValue {
    AmlValue::Integer(if b { ONES } else { 0 })
}

fn bits_to_value(bits: Vec<u8>, bit_len: u32) -> AmlValue {
    if bit_len <= 64 {
        let mut v: u64 = 0;
        for (i, b) in bits.iter().enumerate().take(8) {
            v |= (*b as u64) << (8 * i);
        }
        AmlValue::Integer(v)
    } else {
        AmlValue::Buffer(bits)
    }
}

fn index_value(container: &AmlValue, idx: u64) -> Result<AmlValue, AmlError> {
    let i = idx as usize;
    match container {
        AmlValue::Buffer(b) => b
            .get(i)
            .map(|&x| AmlValue::Integer(x as u64))
            .ok_or(AmlError::IndexOutOfRange),
        AmlValue::String(s) => s
            .as_bytes()
            .get(i)
            .map(|&x| AmlValue::Integer(x as u64))
            .ok_or(AmlError::IndexOutOfRange),
        AmlValue::Package(p) => p.get(i).cloned().ok_or(AmlError::IndexOutOfRange),
        _ => Err(AmlError::TypeMismatch),
    }
}

fn node_type_code(obj: &NodeObject) -> u64 {
    match obj {
        NodeObject::Scope => 0,
        NodeObject::Name(v) => v.object_type(),
        NodeObject::Field(_) | NodeObject::IndexField(_) => 5,
        NodeObject::Device => 6,
        NodeObject::Event => 7,
        NodeObject::Method { .. } | NodeObject::ExternalMethod { .. } => 8,
        NodeObject::Mutex => 9,
        NodeObject::OpRegion { .. } => 10,
        NodeObject::PowerResource => 11,
        NodeObject::Processor => 12,
        NodeObject::ThermalZone => 13,
        NodeObject::BufferField(_) => 14,
        NodeObject::Alias(_) | NodeObject::External => 0,
    }
}

fn to_buffer_bytes(v: &AmlValue) -> Result<Vec<u8>, AmlError> {
    Ok(match v {
        AmlValue::Integer(i) => i.to_le_bytes().to_vec(),
        AmlValue::Buffer(b) => b.clone(),
        AmlValue::String(s) => {
            let mut out: Vec<u8> = s.bytes().collect();
            out.push(0);
            out
        }
        _ => return Err(AmlError::TypeMismatch),
    })
}

/// `ToInteger`'s explicit string conversion: `0x`-prefixed hex or
/// decimal (§19.6.142), unlike the implicit bare-hex rule.
fn parse_explicit_integer(s: &str) -> Result<u64, AmlError> {
    let t = s.trim();
    let (digits, radix) = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(h) => (h, 16),
        None => (t, 10),
    };
    let mut v: u64 = 0;
    let mut any = false;
    for c in digits.bytes() {
        let d = match (c, radix) {
            (b'0'..=b'9', _) => (c - b'0') as u64,
            (b'a'..=b'f', 16) => (c - b'a' + 10) as u64,
            (b'A'..=b'F', 16) => (c - b'A' + 10) as u64,
            _ => break,
        };
        v = v.wrapping_mul(radix).wrapping_add(d);
        any = true;
    }
    if any {
        Ok(v)
    } else {
        Err(AmlError::TypeMismatch)
    }
}

fn to_hex_string(v: &AmlValue) -> Result<String, AmlError> {
    fn hex_u64(x: u64) -> String {
        let mut s = String::from("0x");
        let mut started = false;
        for shift in (0..16).rev() {
            let d = ((x >> (shift * 4)) & 0xF) as u8;
            if d != 0 || started || shift == 0 {
                started = true;
                s.push(if d < 10 {
                    (b'0' + d) as char
                } else {
                    (b'A' + d - 10) as char
                });
            }
        }
        s
    }
    Ok(match v {
        AmlValue::Integer(i) => hex_u64(*i),
        AmlValue::String(s) => s.clone(),
        AmlValue::Buffer(b) => {
            let mut s = String::new();
            for (i, byte) in b.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&hex_u64(*byte as u64));
            }
            s
        }
        _ => return Err(AmlError::TypeMismatch),
    })
}

fn to_decimal_string(v: &AmlValue) -> Result<String, AmlError> {
    fn dec_u64(mut x: u64) -> String {
        if x == 0 {
            return String::from("0");
        }
        let mut digits: Vec<u8> = Vec::new();
        while x > 0 {
            digits.push(b'0' + (x % 10) as u8);
            x /= 10;
        }
        digits.iter().rev().map(|&c| c as char).collect()
    }
    Ok(match v {
        AmlValue::Integer(i) => dec_u64(*i),
        AmlValue::String(s) => s.clone(),
        AmlValue::Buffer(b) => {
            let mut s = String::new();
            for (i, byte) in b.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&dec_u64(*byte as u64));
            }
            s
        }
        _ => return Err(AmlError::TypeMismatch),
    })
}

/// `\_OSI` built-in: answer as a contemporary Windows does (the
/// ACPICA/Linux posture) so firmware takes its vendor-tested branches;
/// everything else — including "Linux" — is unsupported.
fn builtin_osi(arg: Option<&AmlValue>) -> AmlValue {
    const FEATURES: &[&str] = &[
        "Module Device",
        "Processor Device",
        "3.0 Thermal Model",
        "3.0 _SCP Extensions",
        "Processor Aggregator Device",
        "Extended Address Space Descriptor",
    ];
    match arg {
        Some(AmlValue::String(s)) => {
            if s.starts_with("Windows ") || FEATURES.contains(&s.as_str()) {
                AmlValue::Integer(ONES)
            } else {
                AmlValue::Integer(0)
            }
        }
        _ => AmlValue::Integer(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acpi::aml::object::MockRegionSpace;

    /// Wrap a raw AML term-list in a minimal DSDT (36-byte header with
    /// correct length + checksum).
    pub(crate) fn wrap_table(body: &[u8]) -> Vec<u8> {
        let len = 36 + body.len();
        let mut t = Vec::with_capacity(len);
        t.extend_from_slice(b"DSDT");
        t.extend_from_slice(&(len as u32).to_le_bytes());
        t.push(2); // revision ≥ 2 → 64-bit arithmetic
        t.push(0); // checksum patched below
        t.extend_from_slice(b"M3OSTS"); // OEM ID
        t.extend_from_slice(b"FIXTURE_"); // OEM table ID
        t.extend_from_slice(&1u32.to_le_bytes()); // OEM revision
        t.extend_from_slice(b"M3OS"); // creator
        t.extend_from_slice(&1u32.to_le_bytes()); // creator revision
        assert_eq!(t.len(), 36);
        t.extend_from_slice(body);
        let sum: u8 = t.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        t[9] = 0u8.wrapping_sub(sum);
        t
    }

    fn load(body: &[u8]) -> (Namespace, MockRegionSpace) {
        let mut ns = Namespace::new();
        let mut mock = MockRegionSpace::new();
        let table = wrap_table(body);
        let summary = ns.load_table(&table, &mut mock).expect("load");
        assert!(summary.skipped.is_empty(), "skipped: {:?}", summary.skipped);
        (ns, mock)
    }

    // Hand-assembled AML: Method(ADDX, 2) { Return (Arg0 + Arg1) }.
    // MethodOp PkgLength "ADDX" flags=2 ReturnOp AddOp Arg0 Arg1 Null
    #[test]
    fn method_add_evaluates() {
        let body: &[u8] = &[
            0x14, 0x0B, b'A', b'D', b'D', b'X', 0x02, // Method header
            0xA4, 0x72, 0x68, 0x69, 0x00, // Return(Add(Arg0, Arg1, Null))
        ];
        let (mut ns, mut mock) = load(body);
        let m = ns.resolve_str("\\ADDX").expect("method exists");
        let mut interp = Interp::new(&mut ns, &mut mock);
        let r = interp
            .invoke_method(m, alloc::vec![AmlValue::Integer(2), AmlValue::Integer(40)])
            .unwrap();
        assert_eq!(r, AmlValue::Integer(42));
    }

    // Name(CNT0, 0); Method(LOOP) { While (CNT0 < 5) { CNT0++ } Return (CNT0) }
    #[test]
    fn while_loop_with_store() {
        let body: &[u8] = &[
            0x08, b'C', b'N', b'T', b'0', 0x00, // Name(CNT0, Zero)
            0x14, 0x1C, b'L', b'O', b'O', b'P', 0x00, // Method(LOOP, 0)
            0xA2, 0x10, // While, PkgLength=16 (predicate+body)
            0x95, b'C', b'N', b'T', b'0', 0x0A, 0x05, // CNT0 < 5
            0x75, b'C', b'N', b'T', b'0', // Increment(CNT0)
            0xA3, 0xA3, 0xA3, // Noop padding
            0xA4, b'C', b'N', b'T', b'0', // Return (CNT0)
        ];
        let (mut ns, mut mock) = load(body);
        let m = ns.resolve_str("\\LOOP").unwrap();
        let mut interp = Interp::new(&mut ns, &mut mock);
        let r = interp.invoke_method(m, Vec::new()).unwrap();
        assert_eq!(r, AmlValue::Integer(5));
    }

    // If/Else via a method argument.
    // Method(PICK, 1) { If (Arg0) { Return (0x11) } Else { Return (0x22) } }
    #[test]
    fn if_else_branches() {
        let body: &[u8] = &[
            0x14, 0x11, b'P', b'I', b'C', b'K', 0x01, // Method(PICK, 1)
            0xA0, 0x05, 0x68, // If (Arg0) {
            0xA4, 0x0A, 0x11, //   Return (0x11) }
            0xA1, 0x04, // Else {
            0xA4, 0x0A, 0x22, //   Return (0x22) }
        ];
        let (mut ns, mut mock) = load(body);
        let m = ns.resolve_str("\\PICK").unwrap();
        let mut interp = Interp::new(&mut ns, &mut mock);
        assert_eq!(
            interp
                .invoke_method(m, alloc::vec![AmlValue::Integer(1)])
                .unwrap(),
            AmlValue::Integer(0x11)
        );
        let mut interp = Interp::new(&mut ns, &mut mock);
        assert_eq!(
            interp
                .invoke_method(m, alloc::vec![AmlValue::Integer(0)])
                .unwrap(),
            AmlValue::Integer(0x22)
        );
    }

    // Phase 101 D.5 — Device(DEV0) {}; Method(NTFY) { Notify(\DEV0, 0x80) }:
    // a GPE-style method's Notify must land in `pending_notify` carrying
    // the device node + code, ready for acpid's subscriber routing, and
    // `full_path` must render the ASL path subscribers filter against.
    #[test]
    fn notify_records_device_and_code_for_routing() {
        let body: &[u8] = &[
            0x5B, 0x82, 0x05, b'D', b'E', b'V', b'0', // Device(DEV0) {}
            0x14, 0x0E, b'N', b'T', b'F', b'Y', 0x00, // Method(NTFY, 0) {
            0x86, 0x5C, b'D', b'E', b'V', b'0', //   Notify(\DEV0,
            0x0A, 0x80, //     0x80) }
        ];
        let (mut ns, mut mock) = load(body);
        let dev = ns.resolve_str("\\DEV0").expect("device exists");
        assert!(ns.pending_notify.is_empty());
        let m = ns.resolve_str("\\NTFY").expect("method exists");
        let mut interp = Interp::new(&mut ns, &mut mock);
        interp.invoke_method(m, Vec::new()).expect("NTFY evaluates");
        assert_eq!(ns.pending_notify, alloc::vec![(dev, 0x80u64)]);
        assert_eq!(ns.full_path(dev), "\\DEV0");
    }

    // OperationRegion(GPR0, SystemIO, 0x62, 4) + Field → read/write via mock.
    #[test]
    fn opregion_field_round_trip() {
        let body: &[u8] = &[
            // OperationRegion(GPR0, SystemIO(1), 0x62, 0x04)
            0x5B, 0x80, b'G', b'P', b'R', b'0', 0x01, 0x0A, 0x62, 0x0A, 0x04,
            // Field(GPR0, ByteAcc(1), NoLock, Preserve) { STAT, 8, CMDR, 8 }
            0x5B, 0x81, 0x10, b'G', b'P', b'R', b'0', 0x01, //
            b'S', b'T', b'A', b'T', 0x08, //
            b'C', b'M', b'D', b'R', 0x08, //
            // Method(RDST) { Return (STAT) }
            0x14, 0x0B, b'R', b'D', b'S', b'T', 0x00, 0xA4, b'S', b'T', b'A', b'T',
        ];
        let (mut ns, mut mock) = load(body);
        mock.seed(1, 0x62, 8, 0x5A);
        let m = ns.resolve_str("\\RDST").unwrap();
        let mut interp = Interp::new(&mut ns, &mut mock);
        assert_eq!(
            interp.invoke_method(m, Vec::new()).unwrap(),
            AmlValue::Integer(0x5A)
        );
        // Store to the second field unit writes IO port 0x63.
        let cmdr = ns.resolve_str("\\CMDR").unwrap();
        let mut interp = Interp::new(&mut ns, &mut mock);
        interp.store_node(cmdr, AmlValue::Integer(0xA7)).unwrap();
        assert_eq!(mock.read(1, 0x63, 8).unwrap(), 0xA7);
    }

    #[test]
    fn runaway_while_hits_loop_limit() {
        // Method(SPIN) { While (One) { Noop } }
        let body: &[u8] = &[
            0x14, 0x0A, b'S', b'P', b'I', b'N', 0x00, //
            0xA2, 0x03, 0x01, 0xA3, // While(One) { Noop }
        ];
        let (mut ns, mut mock) = load(body);
        let m = ns.resolve_str("\\SPIN").unwrap();
        let mut interp = Interp::new(&mut ns, &mut mock);
        assert_eq!(
            interp.invoke_method(m, Vec::new()),
            Err(AmlError::LoopLimit)
        );
    }

    #[test]
    fn recursive_method_hits_depth_limit() {
        // Method(RECU) { Return (RECU()) }
        let body: &[u8] = &[
            0x14, 0x0B, b'R', b'E', b'C', b'U', 0x00, //
            0xA4, b'R', b'E', b'C', b'U',
        ];
        let (mut ns, mut mock) = load(body);
        let m = ns.resolve_str("\\RECU").unwrap();
        let mut interp = Interp::new(&mut ns, &mut mock);
        assert_eq!(
            interp.invoke_method(m, Vec::new()),
            Err(AmlError::RecursionLimit)
        );
    }

    #[test]
    fn truncated_table_is_error_not_panic() {
        let body: &[u8] = &[
            0x14, 0x0B, b'A', b'D', b'D', b'X', 0x02, //
            0xA4, 0x72, 0x68, 0x69, 0x00,
        ];
        let table = wrap_table(body);
        // Slice the table at every prefix length: must never panic.
        for cut in 0..table.len() {
            let mut ns = Namespace::new();
            let mut mock = MockRegionSpace::new();
            let mut short = table[..cut].to_vec();
            // Keep the declared length consistent with the cut where the
            // header is intact, to exercise deeper failure paths too.
            if cut >= 8 {
                short[4..8].copy_from_slice(&(cut as u32).to_le_bytes());
            }
            let _ = ns.load_table(&short, &mut mock);
        }
    }

    #[test]
    fn osi_builtin_answers_windows() {
        let mut ns = Namespace::new();
        let mut mock = MockRegionSpace::new();
        let osi = ns.resolve_str("\\_OSI").unwrap();
        let mut interp = Interp::new(&mut ns, &mut mock);
        assert_eq!(
            interp
                .invoke_method(
                    osi,
                    alloc::vec![AmlValue::String(String::from("Windows 2015"))]
                )
                .unwrap(),
            AmlValue::Integer(ONES)
        );
        let mut interp = Interp::new(&mut ns, &mut mock);
        assert_eq!(
            interp
                .invoke_method(osi, alloc::vec![AmlValue::String(String::from("Linux"))])
                .unwrap(),
            AmlValue::Integer(0)
        );
    }

    #[test]
    fn package_and_index() {
        // Name(PKGX, Package(3) { 7, "AB", 9 })
        // Method(GETI, 1) { Return (DerefOf(Index(PKGX, Arg0))) }
        let body: &[u8] = &[
            0x08, b'P', b'K', b'G', b'X', // Name(PKGX, ...
            0x12, 0x0A, 0x03, // PackageOp len count=3
            0x0A, 0x07, // 7
            0x0D, b'A', b'B', 0x00, // "AB"
            0x0A, 0x09, // 9
            0x14, 0x0F, b'G', b'E', b'T', b'I', 0x01, // Method(GETI,1)
            0xA4, 0x83, 0x88, b'P', b'K', b'G', b'X', 0x68,
            0x00, // Return(DerefOf(Index(PKGX,Arg0,Null)))
        ];
        let (mut ns, mut mock) = load(body);
        let m = ns.resolve_str("\\GETI").unwrap();
        let mut interp = Interp::new(&mut ns, &mut mock);
        assert_eq!(
            interp
                .invoke_method(m, alloc::vec![AmlValue::Integer(0)])
                .unwrap(),
            AmlValue::Integer(7)
        );
        let mut interp = Interp::new(&mut ns, &mut mock);
        assert_eq!(
            interp
                .invoke_method(m, alloc::vec![AmlValue::Integer(1)])
                .unwrap(),
            AmlValue::String(String::from("AB"))
        );
        let mut interp = Interp::new(&mut ns, &mut mock);
        assert_eq!(
            interp.invoke_method(m, alloc::vec![AmlValue::Integer(9)]),
            Err(AmlError::IndexOutOfRange)
        );
    }
}
