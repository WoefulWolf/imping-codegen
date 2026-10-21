//! Plans layouts: the size and alignment Rust gives each type, and how each
//! struct and union is laid out (padding, bitfield storage, `packed` or
//! `align(N)`), or why it can't be.

use crate::types::{Integer, TypeTable};
use imping_ir::{Field, Primitive, TypeDef, TypeId, TypeKind};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The size and alignment of a type as the generated Rust gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Layout {
    pub(crate) size: u64,
    pub(crate) align: u64,
    /// Whether the type is, or transitively contains, a `#[repr(align(N))]`
    /// type, which a `packed` type may not contain.
    pub(crate) over_aligned: bool,
}

/// The `repr` of a generated struct or union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Repr {
    /// `#[repr(C)]`.
    C,
    /// `#[repr(C, packed)]`: the IR places a member where `repr(C)` can't.
    Packed,
    /// `#[repr(C, align(N))]`: the IR's alignment exceeds the members'.
    Align(u64),
}

/// How a struct, union or opaque fallback is emitted.
#[derive(Debug, Clone)]
pub(crate) enum Plan {
    Struct(StructPlan),
    Union(UnionPlan),
    /// The layout can't be represented; emitted as opaque bytes.
    Opaque {
        size: u64,
        align: u64,
        reason: String,
    },
}

/// The members of a struct, in offset order, with explicit padding.
#[derive(Debug, Clone)]
pub(crate) struct StructPlan {
    pub(crate) repr: Repr,
    /// The alignment the generated struct has.
    pub(crate) align: u64,
    pub(crate) members: Vec<Member>,
}

/// One member of a [`StructPlan`].
#[derive(Debug, Clone)]
pub(crate) enum Member {
    /// An IR field, by index into the struct's fields.
    Field { index: usize, offset: u64 },
    /// Bytes the IR doesn't describe.
    Padding { offset: u64, len: u64 },
    /// Storage shared by the bitfields at one offset.
    Bits {
        offset: u64,
        storage: Integer,
        fields: Vec<BitMember>,
    },
}

/// One bitfield within a [`Member::Bits`] storage unit.
#[derive(Debug, Clone)]
pub(crate) struct BitMember {
    pub(crate) index: usize,
    pub(crate) kind: BitKind,
    pub(crate) bit_offset: u32,
    pub(crate) width: u32,
}

/// What a bitfield's accessors read and write.
#[derive(Debug, Clone, Copy)]
pub(crate) enum BitKind {
    Bool,
    Integer(Integer),
    /// An enum newtype over an integer.
    Enum {
        id: TypeId,
        integer: Integer,
    },
}

impl BitKind {
    fn bytes(self) -> u64 {
        match self {
            BitKind::Bool => 1,
            BitKind::Integer(integer) | BitKind::Enum { integer, .. } => integer.bytes(),
        }
    }

    fn bits(self) -> u32 {
        match self {
            BitKind::Bool => 1,
            BitKind::Integer(integer) | BitKind::Enum { integer, .. } => integer.bits,
        }
    }
}

/// The members of a union, plus a padding member if its fields don't reach
/// the IR's size.
#[derive(Debug, Clone)]
pub(crate) struct UnionPlan {
    pub(crate) repr: Repr,
    /// The alignment the generated union has.
    pub(crate) align: u64,
    pub(crate) fields: Vec<usize>,
    pub(crate) padding: Option<u64>,
}

/// The layout plans of every struct and union in a module.
pub(crate) struct Layouts {
    plans: HashMap<u32, Plan>,
    layouts: HashMap<u32, Option<Layout>>,
}

impl Layouts {
    /// Plans every type in the table.
    pub(crate) fn plan(table: &TypeTable<'_>) -> Self {
        let mut planner = Planner {
            table,
            memo: HashMap::new(),
            visiting: HashSet::new(),
            plans: HashMap::new(),
        };
        for def in &table.module().types {
            planner.layout(def.id);
        }
        Self {
            plans: planner.plans,
            layouts: planner.memo,
        }
    }

    /// Returns the layout of a type, or `None` if it has no size and can't be
    /// used by value.
    pub(crate) fn layout(&self, id: TypeId) -> Option<Layout> {
        self.layouts.get(&id.0).copied().flatten()
    }

    /// Returns the plan for a struct or union.
    pub(crate) fn get(&self, id: TypeId) -> &Plan {
        self.plans
            .get(&id.0)
            .unwrap_or_else(|| panic!("type id {} has no layout plan", id.0))
    }

    /// Returns whether the Rust type for `id` is plain data, valid for every
    /// bit pattern, so it implements the runtime's `Plain`.
    ///
    /// `bool`, `void`, function pointers and unsized opaque types aren't.
    pub(crate) fn is_plain(&self, table: &TypeTable<'_>, id: TypeId) -> bool {
        self.plain_within(table, id, 64)
    }

    fn plain_within(&self, table: &TypeTable<'_>, id: TypeId, depth: usize) -> bool {
        let Some(depth) = depth.checked_sub(1) else {
            return false;
        };
        let def = table.get(id);
        match &def.kind {
            TypeKind::Primitive { primitive } => {
                !matches!(primitive, Primitive::Void | Primitive::Bool)
            }
            TypeKind::Pointer { pointee, .. } => !table
                .peel(*pointee)
                .is_some_and(|pointee| matches!(pointee.kind, TypeKind::Function { .. })),
            TypeKind::Array { element, .. } => self.plain_within(table, *element, depth),
            TypeKind::Struct { fields, .. } | TypeKind::Union { fields, .. } => {
                match self.get(id) {
                    Plan::Opaque { .. } => true,
                    Plan::Struct(plan) => plan.members.iter().all(|member| match member {
                        Member::Field { index, .. } => {
                            self.plain_within(table, fields[*index].type_id, depth)
                        }
                        Member::Padding { .. } | Member::Bits { .. } => true,
                    }),
                    Plan::Union(plan) => plan
                        .fields
                        .iter()
                        .all(|&index| self.plain_within(table, fields[index].type_id, depth)),
                }
            }
            TypeKind::Enum { .. } => true,
            TypeKind::Function { .. } => false,
            TypeKind::Typedef { target } => {
                table.peel(id).is_some() && self.plain_within(table, *target, depth)
            }
            TypeKind::Opaque { size } => size.is_some(),
        }
    }
}

struct Planner<'t, 'a> {
    table: &'t TypeTable<'a>,
    memo: HashMap<u32, Option<Layout>>,
    /// Types whose layout is being computed, to detect by-value cycles.
    visiting: HashSet<u32>,
    plans: HashMap<u32, Plan>,
}

/// A struct member before padding is inserted.
struct Pending<'f> {
    offset: u64,
    layout: Layout,
    label: &'f str,
    member: Member,
}

impl Planner<'_, '_> {
    /// Returns the layout of a type, or `None` if it has no size (`void`,
    /// function types, opaque types of unknown size, by-value cycles).
    fn layout(&mut self, id: TypeId) -> Option<Layout> {
        if let Some(layout) = self.memo.get(&id.0) {
            return *layout;
        }
        if !self.visiting.insert(id.0) {
            return None;
        }
        let def = self.table.get(id);
        let layout = match &def.kind {
            TypeKind::Primitive { primitive } => primitive_layout(*primitive),
            TypeKind::Pointer { .. } => Some(plain(self.table.pointer_size())),
            TypeKind::Array { element, count } => self.layout(*element).and_then(|element| {
                Some(Layout {
                    size: element.size.checked_mul(*count)?,
                    ..element
                })
            }),
            TypeKind::Struct {
                size,
                align,
                fields,
            } => Some(self.plan_struct(def, *size, *align, fields)),
            TypeKind::Union {
                size,
                align,
                fields,
            } => Some(self.plan_union(def, *size, *align, fields)),
            TypeKind::Enum { underlying, .. } => self
                .table
                .integer(*underlying)
                .map(|integer| plain(integer.bytes())),
            TypeKind::Function { .. } => None,
            TypeKind::Typedef { target } => self.table.peel(id).and(self.layout(*target)),
            TypeKind::Opaque { size } => size.map(|size| Layout {
                size,
                align: 1,
                over_aligned: false,
            }),
        };
        self.visiting.remove(&id.0);
        self.memo.insert(id.0, layout);
        layout
    }

    fn plan_struct(&mut self, def: &TypeDef, size: u64, align: u64, fields: &[Field]) -> Layout {
        match self.try_plan_struct(size, align, fields) {
            Ok((plan, layout)) => {
                self.plans.insert(def.id.0, Plan::Struct(plan));
                layout
            }
            Err(reason) => self.opaque(def, size, align, reason),
        }
    }

    fn try_plan_struct(
        &mut self,
        size: u64,
        align: u64,
        fields: &[Field],
    ) -> Result<(StructPlan, Layout), String> {
        let mut pending = Vec::with_capacity(fields.len());
        let mut bit_groups: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        for (index, field) in fields.iter().enumerate() {
            match field.bits {
                Some(bits) if bits.bit_width == 0 => {}
                Some(_) => bit_groups.entry(field.offset).or_default().push(index),
                None => {
                    let layout = self
                        .layout(field.type_id)
                        .ok_or_else(|| format!("field `{}` has no known size", field.name))?;
                    pending.push(Pending {
                        offset: field.offset,
                        layout,
                        label: &field.name,
                        member: Member::Field {
                            index,
                            offset: field.offset,
                        },
                    });
                }
            }
        }

        for (offset, indices) in bit_groups {
            let mut members = Vec::with_capacity(indices.len());
            let mut storage_bytes = 0;
            for &index in &indices {
                let field = &fields[index];
                let bits = field.bits.expect("grouped fields are bitfields");
                let kind = self.bit_kind(field.type_id).ok_or_else(|| {
                    format!("bitfield `{}` is not an integer, bool or enum", field.name)
                })?;
                if bits.bit_width > kind.bits() {
                    return Err(format!("bitfield `{}` is wider than its type", field.name));
                }
                storage_bytes = storage_bytes.max(kind.bytes());
                members.push(BitMember {
                    index,
                    kind,
                    bit_offset: bits.bit_offset,
                    width: bits.bit_width,
                });
            }
            let storage = Integer::unsigned(u32::try_from(storage_bytes * 8).expect("at most 64"));
            if let Some(member) = members.iter().find(|member| {
                u64::from(member.bit_offset) + u64::from(member.width) > u64::from(storage.bits)
            }) {
                return Err(format!(
                    "bitfield `{}` does not fit its {}-bit storage",
                    fields[member.index].name, storage.bits
                ));
            }
            pending.push(Pending {
                offset,
                layout: plain(storage_bytes),
                label: &fields[indices[0]].name,
                member: Member::Bits {
                    offset,
                    storage,
                    fields: members,
                },
            });
        }

        pending.sort_by_key(|item| item.offset);
        let mut end = 0;
        let mut previous: Option<&str> = None;
        for item in &pending {
            if item.offset < end {
                return Err(format!(
                    "field `{}` overlaps `{}`",
                    item.label,
                    previous.unwrap_or_default()
                ));
            }
            end = item
                .offset
                .checked_add(item.layout.size)
                .ok_or_else(|| format!("field `{}` extends past the end of memory", item.label))?;
            previous = Some(item.label);
        }
        if end > size {
            return Err(format!(
                "field `{}` extends past the type's size of {size} bytes",
                previous.unwrap_or_default()
            ));
        }

        let natural = pending
            .iter()
            .map(|item| item.layout.align)
            .max()
            .unwrap_or(1)
            .max(1);
        let over_aligned = pending.iter().any(|item| item.layout.over_aligned);
        let ir_align = if align.is_power_of_two() { align } else { 1 };
        let target = natural.max(ir_align);
        let repr_c = pending
            .iter()
            .all(|item| item.offset.is_multiple_of(item.layout.align.max(1)))
            && size.is_multiple_of(target);
        let (repr, final_align) = if repr_c {
            if ir_align > natural {
                (Repr::Align(ir_align), ir_align)
            } else {
                (Repr::C, natural)
            }
        } else if over_aligned {
            return Err("it needs `packed`, but contains an over-aligned type".to_owned());
        } else {
            (Repr::Packed, 1)
        };

        let mut members = Vec::with_capacity(pending.len() * 2 + 1);
        let mut cursor = 0;
        for item in pending {
            if item.offset > cursor {
                members.push(Member::Padding {
                    offset: cursor,
                    len: item.offset - cursor,
                });
            }
            cursor = item.offset + item.layout.size;
            members.push(item.member);
        }
        if size > cursor {
            members.push(Member::Padding {
                offset: cursor,
                len: size - cursor,
            });
        }

        let layout = Layout {
            size,
            align: final_align,
            over_aligned: over_aligned || matches!(repr, Repr::Align(_)),
        };
        let plan = StructPlan {
            repr,
            align: final_align,
            members,
        };
        Ok((plan, layout))
    }

    fn plan_union(&mut self, def: &TypeDef, size: u64, align: u64, fields: &[Field]) -> Layout {
        match self.try_plan_union(size, align, fields) {
            Ok((plan, layout)) => {
                self.plans.insert(def.id.0, Plan::Union(plan));
                layout
            }
            Err(reason) => self.opaque(def, size, align, reason),
        }
    }

    fn try_plan_union(
        &mut self,
        size: u64,
        align: u64,
        fields: &[Field],
    ) -> Result<(UnionPlan, Layout), String> {
        let mut max_size = 0;
        let mut natural = 1;
        let mut over_aligned = false;
        for field in fields {
            if field.bits.is_some() {
                return Err(format!(
                    "bitfield `{}` in a union is not supported",
                    field.name
                ));
            }
            if field.offset != 0 {
                return Err(format!("member `{}` is not at offset 0", field.name));
            }
            let layout = self
                .layout(field.type_id)
                .ok_or_else(|| format!("member `{}` has no known size", field.name))?;
            if layout.size > size {
                return Err(format!("member `{}` is larger than the union", field.name));
            }
            max_size = max_size.max(layout.size);
            natural = natural.max(layout.align);
            over_aligned |= layout.over_aligned;
        }

        let ir_align = if align.is_power_of_two() { align } else { 1 };
        let target = natural.max(ir_align);
        let (repr, final_align) = if size.is_multiple_of(target) {
            if ir_align > natural {
                (Repr::Align(ir_align), ir_align)
            } else {
                (Repr::C, natural)
            }
        } else if over_aligned {
            return Err("it needs `packed`, but contains an over-aligned type".to_owned());
        } else {
            (Repr::Packed, 1)
        };
        let rust_size = max_size.next_multiple_of(final_align);
        let padding = (rust_size != size || fields.is_empty()).then_some(size);

        let layout = Layout {
            size,
            align: final_align,
            over_aligned: over_aligned || matches!(repr, Repr::Align(_)),
        };
        let plan = UnionPlan {
            repr,
            align: final_align,
            fields: (0..fields.len()).collect(),
            padding,
        };
        Ok((plan, layout))
    }

    /// Records an opaque-bytes fallback for a struct or union.
    fn opaque(&mut self, def: &TypeDef, size: u64, align: u64, reason: String) -> Layout {
        let align = if align.is_power_of_two() && align > 1 && size.is_multiple_of(align) {
            align
        } else {
            1
        };
        self.plans.insert(
            def.id.0,
            Plan::Opaque {
                size,
                align,
                reason,
            },
        );
        Layout {
            size,
            align,
            over_aligned: align > 1,
        }
    }

    /// Returns what a bitfield of this declared type reads and writes, or
    /// `None` if it can't be a bitfield.
    fn bit_kind(&self, id: TypeId) -> Option<BitKind> {
        let def = self.table.peel(id)?;
        match def.kind {
            TypeKind::Primitive {
                primitive: Primitive::Bool,
            } => Some(BitKind::Bool),
            TypeKind::Primitive { primitive } => {
                Integer::from_primitive(primitive).map(BitKind::Integer)
            }
            TypeKind::Enum { underlying, .. } => Some(BitKind::Enum {
                id: def.id,
                integer: self.table.integer(underlying)?,
            }),
            _ => None,
        }
    }
}

/// Returns the layout of a type whose alignment equals its size.
fn plain(size: u64) -> Layout {
    Layout {
        size,
        align: size,
        over_aligned: false,
    }
}

fn primitive_layout(primitive: Primitive) -> Option<Layout> {
    let size = match primitive {
        Primitive::Void => return None,
        Primitive::Bool | Primitive::U8 | Primitive::I8 => 1,
        Primitive::U16 | Primitive::I16 => 2,
        Primitive::U32 | Primitive::I32 | Primitive::F32 => 4,
        Primitive::U64 | Primitive::I64 | Primitive::F64 => 8,
    };
    Some(plain(size))
}
