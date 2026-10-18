//! Layer 1: a generic walk of any typedstream into an index-based tree
//! (M3 §10.3–§10.5).
//!
//! - **Shared strings** are spans of the input, numbered in order of first
//!   appearance. A type string is parsed once per shared-string index and
//!   cached.
//! - **The object table** holds classes, objects and `char *` pointers, in
//!   order of first appearance (T1). A reference is the head read as a
//!   signed integer plus 110; it is bounds-checked, and a reference to a
//!   slot still under construction is `BadReference`, so the tree has no
//!   cycles.
//! - **Objects** are walked generically: value groups until `END`. Field
//!   names are not stored in the format; a group is a type plus its values.
//!   References stay references and are never expanded.
//!
//! Recursion is bounded: objects nest at most `max_depth` deep, and a
//! struct's nesting adds to the depth of the object holding it. Superclass
//! chains are read iteratively. Every type element read is charged to
//! `max_nodes`, zero-width ones included, so a stream's work is bounded by
//! the budget rather than by its length. A type string is parsed into at
//! most the nodes still left in the budget, so the element arena is bounded
//! by `max_nodes` before it is filled, not after.
//!
//! **Class kinds are resolved once.** When a superclass chain is complete,
//! each new class records the first [`Kind`] on its chain (its own name, or
//! its superclass's kind), so layer 2's class lookup is O(1) per object,
//! however long the chain and however often the object is referenced.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unreachable,
    clippy::unimplemented,
    clippy::string_slice
)]

use super::foundation::{self, Kind};
use super::reader::{ref_index, Reader, Span, END, NEW, NIL};
use super::stats::Meter;
use super::types::{self, Elem};
use super::{TsError, TsErrorKind};

/// An index into the object table.
pub(crate) type SlotId = u32;
/// An index into the shared-string table.
pub(crate) type SharedId = u32;

/// One value read from a group.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Val {
    Signed(i64),
    Unsigned(u64),
    Real(f64),
    Bool(u8),
    /// `+` or `[Nc]` bytes.
    Bytes(Span),
    CStr(Option<SlotId>),
    Shared(Option<SharedId>),
    Class(Option<SlotId>),
    Object(Option<SlotId>),
}

/// One entry of the object table.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Slot {
    /// Reserved, still under construction.
    Pending,
    Class {
        name: SharedId,
        superclass: Option<SlotId>,
    },
    CStr {
        text: SharedId,
    },
    Object {
        class: SlotId,
        groups: (u32, u32),
    },
}

/// A parsed type string.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TypeInfo {
    /// The shared string it came from.
    pub(crate) shared: SharedId,
    elems: (u32, u32),
    struct_depth: u16,
}

/// One value group: a type and its values.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Group {
    pub(crate) ty: u32,
    pub(crate) vals: (u32, u32),
}

const NOT_PARSED: u32 = u32::MAX;

/// The walked stream.
pub(crate) struct Doc<'a> {
    pub(crate) input: &'a [u8],
    shared: Vec<Span>,
    type_of_shared: Vec<u32>,
    types: Vec<TypeInfo>,
    elems: Vec<Elem>,
    slots: Vec<Slot>,
    /// One entry per slot: a class slot's resolved kind, `Other` for every
    /// other slot.
    class_kinds: Vec<Kind>,
    groups: Vec<Group>,
    vals: Vec<Val>,
}

impl<'a> Doc<'a> {
    /// A class slot's kind: the first known name on its superclass chain,
    /// most derived first, resolved when the chain was read.
    pub(crate) fn class_kind(&self, id: SlotId) -> Kind {
        usize::try_from(id)
            .ok()
            .and_then(|i| self.class_kinds.get(i))
            .copied()
            .unwrap_or(Kind::Other)
    }

    pub(crate) fn slot_count(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn slot(&self, id: SlotId) -> Option<Slot> {
        self.slots.get(usize::try_from(id).ok()?).copied()
    }

    pub(crate) fn shared_bytes(&self, id: SharedId) -> Option<&'a [u8]> {
        let s = *self.shared.get(usize::try_from(id).ok()?)?;
        self.span(s)
    }

    pub(crate) fn span(&self, s: Span) -> Option<&'a [u8]> {
        self.input
            .get(usize::try_from(s.start).ok()?..usize::try_from(s.end).ok()?)
    }

    /// The groups of an object slot.
    pub(crate) fn object_groups(&self, obj: SlotId) -> Option<&[Group]> {
        match self.slot(obj)? {
            Slot::Object { groups, .. } => self.groups.get(range(groups)?),
            _ => None,
        }
    }

    /// An object's class.
    pub(crate) fn object_class(&self, obj: SlotId) -> Option<SlotId> {
        match self.slot(obj)? {
            Slot::Object { class, .. } => Some(class),
            _ => None,
        }
    }

    pub(crate) fn group(&self, idx: u32) -> Option<Group> {
        self.groups.get(usize::try_from(idx).ok()?).copied()
    }

    pub(crate) fn values(&self, g: Group) -> Option<&[Val]> {
        self.vals.get(range(g.vals)?)
    }

    /// The type string of a group.
    pub(crate) fn type_bytes(&self, g: Group) -> Option<&'a [u8]> {
        let t = self.types.get(usize::try_from(g.ty).ok()?)?;
        self.shared_bytes(t.shared)
    }

    /// The single value of a group whose type string is exactly `ty`.
    pub(crate) fn single(&self, g: Group, ty: &[u8]) -> Option<Val> {
        if self.type_bytes(g)? != ty {
            return None;
        }
        match self.values(g)? {
            [v] => Some(*v),
            _ => None,
        }
    }

    /// The text of a `char *` slot.
    pub(crate) fn cstr_bytes(&self, id: SlotId) -> Option<&'a [u8]> {
        match self.slot(id)? {
            Slot::CStr { text } => self.shared_bytes(text),
            _ => None,
        }
    }
}

fn range(r: (u32, u32)) -> Option<std::ops::Range<usize>> {
    Some(usize::try_from(r.0).ok()?..usize::try_from(r.1).ok()?)
}

fn len_u32(n: usize, at: usize) -> Result<u32, TsError> {
    u32::try_from(n).map_err(|_| TsError::new(TsErrorKind::Budget, at))
}

/// The walker. Values and groups of the objects under construction live on
/// two stacks; when an object ends, its own entries move, contiguous, into
/// the document, and the stacks are truncated back.
pub(crate) struct Walker<'a, 'm> {
    r: Reader<'a>,
    doc: Doc<'a>,
    meter: &'m mut Meter,
    stack_vals: Vec<Val>,
    stack_groups: Vec<Group>,
    chain: Vec<(SlotId, SharedId)>,
}

impl<'a, 'm> Walker<'a, 'm> {
    pub(crate) fn new(input: &'a [u8], meter: &'m mut Meter) -> Self {
        Walker {
            r: Reader::new(input),
            doc: Doc {
                input,
                shared: Vec::new(),
                type_of_shared: Vec::new(),
                types: Vec::new(),
                elems: Vec::new(),
                slots: Vec::new(),
                class_kinds: Vec::new(),
                groups: Vec::new(),
                vals: Vec::new(),
            },
            meter,
            stack_vals: Vec::new(),
            stack_groups: Vec::new(),
            chain: Vec::new(),
        }
    }

    pub(crate) fn header(&mut self) -> Result<(), TsError> {
        self.r.header()
    }

    pub(crate) fn pos(&self) -> usize {
        self.r.pos()
    }

    pub(crate) fn at_end(&self) -> bool {
        self.r.at_end()
    }

    pub(crate) fn into_doc(self) -> Doc<'a> {
        self.doc
    }

    /// Read one top-level group; return its index in the document.
    pub(crate) fn top_group(&mut self) -> Result<u32, TsError> {
        let gs = self.stack_groups.len();
        let vs = self.stack_vals.len();
        let ty = self.read_type()?;
        self.read_values(ty, 0)?;
        let at = self.r.pos();
        let (g0, _) = self.commit(gs, vs, at)?;
        Ok(g0)
    }

    /// Move the stack entries from `gs`/`vs` on into the document.
    fn commit(&mut self, gs: usize, vs: usize, at: usize) -> Result<(u32, u32), TsError> {
        let vbase = len_u32(self.doc.vals.len(), at)?;
        let vstart = len_u32(vs, at)?;
        let g0 = len_u32(self.doc.groups.len(), at)?;
        if let Some(vals) = self.stack_vals.get(vs..) {
            self.doc.vals.extend_from_slice(vals);
        }
        if let Some(groups) = self.stack_groups.get(gs..) {
            for g in groups {
                let shift = |x: u32| x.saturating_sub(vstart).saturating_add(vbase);
                self.doc.groups.push(Group {
                    ty: g.ty,
                    vals: (shift(g.vals.0), shift(g.vals.1)),
                });
            }
        }
        self.stack_vals.truncate(vs);
        self.stack_groups.truncate(gs);
        let g1 = len_u32(self.doc.groups.len(), at)?;
        Ok((g0, g1))
    }

    fn new_slot(&mut self, at: usize) -> Result<SlotId, TsError> {
        self.meter.slot(at)?;
        let id = len_u32(self.doc.slots.len(), at)?;
        self.doc.slots.push(Slot::Pending);
        self.doc.class_kinds.push(Kind::Other);
        Ok(id)
    }

    fn set_slot(&mut self, id: SlotId, slot: Slot) {
        if let Some(s) = usize::try_from(id)
            .ok()
            .and_then(|i| self.doc.slots.get_mut(i))
        {
            *s = slot;
        }
    }

    /// A reference to a completed slot, after its head byte was read.
    fn slot_ref(&mut self, head: u8, at: usize) -> Result<(SlotId, Slot), TsError> {
        let v = self.r.signed(head)?;
        let bad = TsError::new(TsErrorKind::BadReference, at);
        let id = ref_index(v).ok_or(bad)?;
        match self.doc.slot(id) {
            None | Some(Slot::Pending) => Err(bad),
            Some(s) => Ok((id, s)),
        }
    }

    /// A shared string: new, nil or a reference.
    fn read_shared(&mut self) -> Result<Option<SharedId>, TsError> {
        let at = self.r.pos();
        let head = self.r.byte()?;
        match head {
            NEW => {
                let n = self.r.length()?;
                let span = self.r.take(n)?;
                self.meter.shared(at)?;
                let id = len_u32(self.doc.shared.len(), at)?;
                self.doc.shared.push(span);
                self.doc.type_of_shared.push(NOT_PARSED);
                Ok(Some(id))
            }
            NIL => Ok(None),
            _ => {
                let v = self.r.signed(head)?;
                let bad = TsError::new(TsErrorKind::BadReference, at);
                let id = ref_index(v).ok_or(bad)?;
                if usize::try_from(id).map_err(|_| bad)? < self.doc.shared.len() {
                    Ok(Some(id))
                } else {
                    Err(bad)
                }
            }
        }
    }

    /// A type: a shared string, parsed once per shared index.
    fn read_type(&mut self) -> Result<u32, TsError> {
        let at = self.r.pos();
        let id = self
            .read_shared()?
            .ok_or(TsError::new(TsErrorKind::UnexpectedTag, at))?;
        let idx = usize::try_from(id).map_err(|_| TsError::new(TsErrorKind::Budget, at))?;
        if let Some(&t) = self.doc.type_of_shared.get(idx) {
            if t != NOT_PARSED {
                return Ok(t);
            }
        }
        let bytes = self
            .doc
            .shared_bytes(id)
            .ok_or(TsError::new(TsErrorKind::BadReference, at))?;
        let e0 = len_u32(self.doc.elems.len(), at)?;
        // Every element parsed here is charged to the node meter when its
        // group is read, so a type string longer than the nodes left can only
        // fail; refuse it before filling the arena.
        let cap = self
            .doc
            .elems
            .len()
            .saturating_add(usize::try_from(self.meter.nodes_left()).unwrap_or(usize::MAX));
        let parsed = types::parse(
            bytes,
            self.meter.budget().max_depth(),
            cap,
            &mut self.doc.elems,
        )
        .map_err(|k| TsError::new(k, at))?;
        self.meter.type_parsed();
        let e1 = len_u32(self.doc.elems.len(), at)?;
        let t = len_u32(self.doc.types.len(), at)?;
        self.doc.types.push(TypeInfo {
            shared: id,
            elems: (e0, e1),
            struct_depth: parsed.struct_depth,
        });
        if let Some(slot) = self.doc.type_of_shared.get_mut(idx) {
            *slot = t;
        }
        Ok(t)
    }

    /// The values of one group of type `ty`, inside an object at
    /// `obj_depth` (0 at the top level). Pushes them and the group record
    /// onto the stacks.
    fn read_values(&mut self, ty: u32, obj_depth: u32) -> Result<(), TsError> {
        let at = self.r.pos();
        let info = *self
            .doc
            .types
            .get(usize::try_from(ty).map_err(|_| TsError::new(TsErrorKind::Budget, at))?)
            .ok_or(TsError::new(TsErrorKind::BadReference, at))?;
        let depth = obj_depth.saturating_add(u32::from(info.struct_depth));
        self.meter.depth(depth, at)?;
        let v0 = len_u32(self.stack_vals.len(), at)?;
        let (e0, e1) = info.elems;
        let mut i = e0;
        while i < e1 {
            let elem = self
                .doc
                .elems
                .get(usize::try_from(i).map_err(|_| TsError::new(TsErrorKind::Budget, at))?)
                .copied()
                .ok_or(TsError::new(TsErrorKind::Budget, at))?;
            i = i.saturating_add(1);
            let here = self.r.pos();
            self.meter.nodes(1, here)?;
            let v = match elem {
                Elem::Struct => continue,
                Elem::Signed => {
                    let h = self.r.byte()?;
                    Val::Signed(self.r.signed(h)?)
                }
                Elem::Unsigned => {
                    let h = self.r.byte()?;
                    Val::Unsigned(self.r.unsigned(h)?)
                }
                Elem::Float | Elem::Double => {
                    let h = self.r.byte()?;
                    Val::Real(self.r.real(h, elem == Elem::Double)?)
                }
                Elem::Bool => Val::Bool(self.r.byte()?),
                Elem::Unshared => {
                    let n = self.r.length()?;
                    Val::Bytes(self.r.take(n)?)
                }
                Elem::Bytes(n) => Val::Bytes(self.r.take(n)?),
                Elem::CStr => Val::CStr(self.read_cstr()?),
                Elem::Shared => Val::Shared(self.read_shared()?),
                Elem::Class => Val::Class(self.read_class()?),
                Elem::Object => Val::Object(self.read_object(obj_depth.saturating_add(1))?),
            };
            self.stack_vals.push(v);
        }
        let v1 = len_u32(self.stack_vals.len(), at)?;
        self.stack_groups.push(Group { ty, vals: (v0, v1) });
        Ok(())
    }

    /// A `char *`: nil, a new pointer slot holding a shared string, or a
    /// reference to a pointer slot.
    fn read_cstr(&mut self) -> Result<Option<SlotId>, TsError> {
        let at = self.r.pos();
        let head = self.r.byte()?;
        match head {
            NIL => Ok(None),
            NEW => {
                let id = self.new_slot(at)?;
                let text = self
                    .read_shared()?
                    .ok_or(TsError::new(TsErrorKind::UnexpectedTag, at))?;
                self.set_slot(id, Slot::CStr { text });
                Ok(Some(id))
            }
            _ => match self.slot_ref(head, at)? {
                (id, Slot::CStr { .. }) => Ok(Some(id)),
                _ => Err(TsError::new(TsErrorKind::BadReference, at)),
            },
        }
    }

    /// A class: nil, a reference to a completed class, or new classes up a
    /// superclass chain that ends in nil or a reference. The chain is read
    /// iteratively; its slots stay pending until it ends, so a chain cannot
    /// refer to itself.
    fn read_class(&mut self) -> Result<Option<SlotId>, TsError> {
        self.chain.clear();
        let end = loop {
            let at = self.r.pos();
            let head = self.r.byte()?;
            match head {
                NEW => {
                    let id = self.new_slot(at)?;
                    let name = self
                        .read_shared()?
                        .ok_or(TsError::new(TsErrorKind::UnexpectedTag, at))?;
                    let h = self.r.byte()?;
                    let _version = self.r.signed(h)?;
                    self.chain.push((id, name));
                }
                NIL => break None,
                _ => match self.slot_ref(head, at)? {
                    (id, Slot::Class { .. }) => break Some(id),
                    _ => return Err(TsError::new(TsErrorKind::BadReference, at)),
                },
            }
        };
        let mut superclass = end;
        let mut kind = end.map_or(Kind::Other, |c| self.doc.class_kind(c));
        let mut i = self.chain.len();
        while let Some(prev) = i.checked_sub(1) {
            i = prev;
            if let Some(&(id, name)) = self.chain.get(i) {
                self.set_slot(id, Slot::Class { name, superclass });
                kind = self
                    .doc
                    .shared_bytes(name)
                    .and_then(foundation::base_kind)
                    .unwrap_or(kind);
                if let Some(k) = usize::try_from(id)
                    .ok()
                    .and_then(|x| self.doc.class_kinds.get_mut(x))
                {
                    *k = kind;
                }
                superclass = Some(id);
            }
        }
        Ok(superclass)
    }

    /// An object at `depth` (1 for the root): nil, a reference to a
    /// completed object, or a new object with its class and value groups.
    fn read_object(&mut self, depth: u32) -> Result<Option<SlotId>, TsError> {
        let at = self.r.pos();
        let head = self.r.byte()?;
        match head {
            NIL => Ok(None),
            NEW => {
                self.meter.depth(depth, at)?;
                let id = self.new_slot(at)?;
                let class = self
                    .read_class()?
                    .ok_or(TsError::new(TsErrorKind::UnexpectedTag, at))?;
                let gs = self.stack_groups.len();
                let vs = self.stack_vals.len();
                loop {
                    if self.r.peek()? == END {
                        self.r.byte()?;
                        break;
                    }
                    let ty = self.read_type()?;
                    self.read_values(ty, depth)?;
                }
                let groups = self.commit(gs, vs, at)?;
                self.set_slot(id, Slot::Object { class, groups });
                Ok(Some(id))
            }
            _ => match self.slot_ref(head, at)? {
                (id, Slot::Object { .. }) => Ok(Some(id)),
                _ => Err(TsError::new(TsErrorKind::BadReference, at)),
            },
        }
    }
}

/// Walk every top-level group (for the generic vector tests).
#[cfg(test)]
pub(crate) fn walk_all<'a>(
    input: &'a [u8],
    meter: &mut Meter,
) -> Result<(Doc<'a>, Vec<u32>), TsError> {
    let mut w = Walker::new(input, meter);
    w.header()?;
    let mut tops = Vec::new();
    while !w.at_end() {
        tops.push(w.top_group()?);
    }
    Ok((w.into_doc(), tops))
}
