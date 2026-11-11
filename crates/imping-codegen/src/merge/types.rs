//! The merged type table: matching each build's types to it.

use super::diff::{describe, differ, kind_difference};
use crate::emit::qualified;
use crate::error::Error;
use imping_ir::{Function, Module, TypeDef, TypeId, TypeKind};
use std::collections::{HashMap, HashSet};

/// What identifies a named type across builds: namespace, name, kind, and
/// which of the types with those it is, in id order.
type TypeKey = (Vec<String>, String, &'static str, usize);

/// What identifies a pointer, array or function type: what it's built from,
/// in merged ids.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Shape {
    Pointer(TypeId, bool),
    Array(TypeId, u64),
    /// Calling convention (as its `Debug` text), parameters, return type and
    /// whether it's variadic.
    Function(String, Vec<TypeId>, TypeId, bool),
}

impl Shape {
    /// The shape of a pointer, array or function type, `None` for any other.
    fn of(kind: &TypeKind) -> Option<Self> {
        match kind {
            TypeKind::Pointer { pointee, is_const } => Some(Shape::Pointer(*pointee, *is_const)),
            TypeKind::Array { element, count } => Some(Shape::Array(*element, *count)),
            TypeKind::Function {
                calling_convention,
                parameters,
                return_type,
                variadic,
            } => Some(Shape::Function(
                format!("{calling_convention:?}"),
                parameters.clone(),
                *return_type,
                *variadic,
            )),
            _ => None,
        }
    }
}

/// The types a pointer, array or function type is built from.
fn parts(kind: &TypeKind) -> Vec<TypeId> {
    match kind {
        TypeKind::Pointer { pointee, .. } => vec![*pointee],
        TypeKind::Array { element, .. } => vec![*element],
        TypeKind::Function {
            parameters,
            return_type,
            ..
        } => parameters.iter().chain([return_type]).copied().collect(),
        _ => Vec::new(),
    }
}

/// The merged type table, and how each build's type ids map into it.
pub(super) struct Types {
    pub(super) merged: Vec<TypeDef>,
    /// Where each merged type is in `merged`.
    index: HashMap<TypeId, usize>,
    /// Named types by key.
    by_key: HashMap<TypeKey, TypeId>,
    /// Pointer, array and function types by shape, in canonical ids.
    by_shape: HashMap<Shape, TypeId>,
    /// Pointer, array and function types to the one merged type of their
    /// shape they're compared as; other types are their own.
    canon: HashMap<TypeId, TypeId>,
    /// Per build, in label order: its type ids to merged ones.
    pub(super) maps: Vec<HashMap<TypeId, TypeId>>,
    /// The label of the build each merged type came from, by id.
    origin: HashMap<TypeId, String>,
    next_id: u32,
}

/// What a build's types become in the merged table when they match none of
/// it.
#[derive(Clone, Copy)]
enum Ids<'l> {
    /// The first build's: they keep their ids.
    Kept,
    /// A later build's, labelled so: they're added under new ids.
    Added(&'l str),
}

impl Types {
    /// Starts from the first build's types, keeping their ids.
    pub(super) fn new(first: &Module) -> Self {
        let next_id = first
            .types
            .iter()
            .map(|def| def.id.0 + 1)
            .max()
            .unwrap_or(0);
        let index = first
            .types
            .iter()
            .enumerate()
            .map(|(position, def)| (def.id, position))
            .collect();
        let mut types = Self {
            merged: first.types.clone(),
            index,
            by_key: HashMap::new(),
            by_shape: HashMap::new(),
            canon: HashMap::new(),
            maps: Vec::new(),
            origin: HashMap::new(),
            next_id,
        };
        let mut map = HashMap::new();
        for (key, def) in type_keys(first) {
            if Shape::of(&def.kind).is_none() {
                types.by_key.insert(key, def.id);
                map.insert(def.id, def.id);
            }
        }
        types.map_shapes(first, Ids::Kept, &mut map);
        types.maps.push(map);
        types
    }

    /// Adds a later build's types: each matches a type of the same key or
    /// shape, which it must equal, or is added.
    pub(super) fn add(
        &mut self,
        first: &Module,
        first_label: &str,
        label: &str,
        module: &Module,
    ) -> Result<(), Error> {
        let keyed = type_keys(module);
        let mut map = HashMap::new();
        let mut added = Vec::new();
        for (key, def) in &keyed {
            if Shape::of(&def.kind).is_some() {
                continue;
            }
            let id = match self.by_key.get(key) {
                Some(&id) => id,
                None => {
                    let id = self.own_id(def, Ids::Added(label), &mut added);
                    self.by_key.insert(key.clone(), id);
                    id
                }
            };
            map.insert(def.id, id);
        }
        added.extend(self.map_shapes(module, Ids::Added(label), &mut map));

        let added_ids: HashSet<TypeId> = added.iter().map(|(id, _)| *id).collect();
        for (id, def) in added {
            self.index.insert(id, self.merged.len());
            self.merged.push(TypeDef {
                id,
                kind: remap_kind(&def.kind, |id| map[&id]),
                ..def.clone()
            });
        }
        for (_, def) in &keyed {
            let id = map[&def.id];
            if added_ids.contains(&id) || Shape::of(&def.kind).is_some() {
                continue;
            }
            let kind = remap_kind(&def.kind, |id| map[&id]);
            let known = self.canonical_kind(&self.merged[self.index[&id]].kind);
            if let Some(difference) = kind_difference(&known, &kind, &self.merged) {
                let from = self.origin.get(&id).map_or(first_label, String::as_str);
                return Err(differ(
                    first,
                    format!("type `{}`", qualified(&def.namespace, &def.name)),
                    from,
                    label,
                    difference,
                ));
            }
        }
        self.maps.push(map);
        Ok(())
    }

    /// Maps the pointer, array and function types of `module` into `map`,
    /// which has its named types already, and returns the types it added.
    fn map_shapes<'m>(
        &mut self,
        module: &'m Module,
        ids: Ids<'_>,
        map: &mut HashMap<TypeId, TypeId>,
    ) -> Vec<(TypeId, &'m TypeDef)> {
        let defs: HashMap<TypeId, &TypeDef> =
            module.types.iter().map(|def| (def.id, def)).collect();
        let mut added = Vec::new();
        let mut visiting = HashSet::new();
        for (_, def) in type_keys(module) {
            if Shape::of(&def.kind).is_some() {
                self.map_shaped(def, ids, &defs, map, &mut added, &mut visiting);
            }
        }
        added
    }

    /// Maps one pointer, array or function type, after the types it's built
    /// from. It's compared as the merged type of its shape; with no such type
    /// yet, it becomes that type.
    fn map_shaped<'m>(
        &mut self,
        def: &'m TypeDef,
        ids: Ids<'_>,
        defs: &HashMap<TypeId, &'m TypeDef>,
        map: &mut HashMap<TypeId, TypeId>,
        added: &mut Vec<(TypeId, &'m TypeDef)>,
        visiting: &mut HashSet<TypeId>,
    ) {
        if map.contains_key(&def.id) {
            return;
        }
        if !visiting.insert(def.id) {
            // Built from itself, which valid IR never is: it's its own type.
            let id = self.own_id(def, ids, added);
            self.canon.insert(id, id);
            map.insert(def.id, id);
            return;
        }
        for part in parts(&def.kind) {
            if let Some(part) = defs
                .get(&part)
                .filter(|part| Shape::of(&part.kind).is_some())
            {
                self.map_shaped(part, ids, defs, map, added, visiting);
            }
        }
        visiting.remove(&def.id);
        if map.contains_key(&def.id) {
            return;
        }
        let kind = remap_kind(&def.kind, |id| self.canonical(map[&id]));
        let shape = Shape::of(&kind).expect("only shaped types get here");
        let id = match (self.by_shape.get(&shape).copied(), ids) {
            // The first build's types all stay, so a duplicate keeps its id
            // and is compared as the first of its shape.
            (Some(canonical), Ids::Kept) => {
                self.canon.insert(def.id, canonical);
                def.id
            }
            (Some(canonical), Ids::Added(_)) => canonical,
            (None, _) => {
                let id = self.own_id(def, ids, added);
                self.by_shape.insert(shape, id);
                self.canon.insert(id, id);
                id
            }
        };
        map.insert(def.id, id);
    }

    /// The merged id of a type that matches none: its own for the first
    /// build, a new one for a later build, which is added.
    fn own_id<'m>(
        &mut self,
        def: &'m TypeDef,
        ids: Ids<'_>,
        added: &mut Vec<(TypeId, &'m TypeDef)>,
    ) -> TypeId {
        match ids {
            Ids::Kept => def.id,
            Ids::Added(label) => {
                let id = TypeId(self.next_id);
                self.next_id += 1;
                self.origin.insert(id, label.to_owned());
                added.push((id, def));
                id
            }
        }
    }

    /// The merged type `id` is compared as.
    pub(super) fn canonical(&self, id: TypeId) -> TypeId {
        self.canon.get(&id).copied().unwrap_or(id)
    }

    fn canonical_kind(&self, kind: &TypeKind) -> TypeKind {
        remap_kind(kind, |id| self.canonical(id))
    }

    pub(super) fn canonical_function(&self, function: &Function) -> Function {
        remap_function(function, |id| self.canonical(id))
    }

    pub(super) fn describe(&self, id: TypeId) -> String {
        describe(&self.merged, id)
    }
}

/// Each of a module's types with its key, in id order.
fn type_keys(module: &Module) -> Vec<(TypeKey, &TypeDef)> {
    let mut order: Vec<&TypeDef> = module.types.iter().collect();
    order.sort_by_key(|def| def.id.0);
    let mut seen: HashMap<(&[String], &str, &'static str), usize> = HashMap::new();
    order
        .into_iter()
        .map(|def| {
            let tag = kind_tag(&def.kind);
            let count = seen
                .entry((def.namespace.as_slice(), def.name.as_str(), tag))
                .or_default();
            let key = (def.namespace.clone(), def.name.clone(), tag, *count);
            *count += 1;
            (key, def)
        })
        .collect()
}

pub(super) fn kind_tag(kind: &TypeKind) -> &'static str {
    match kind {
        TypeKind::Primitive { .. } => "a primitive",
        TypeKind::Pointer { .. } => "a pointer",
        TypeKind::Array { .. } => "an array",
        TypeKind::Struct { .. } => "a struct",
        TypeKind::Union { .. } => "a union",
        TypeKind::Enum { .. } => "an enum",
        TypeKind::Function { .. } => "a function type",
        TypeKind::Typedef { .. } => "a typedef",
        TypeKind::Opaque { .. } => "an opaque type",
    }
}

fn remap_kind(kind: &TypeKind, map: impl Fn(TypeId) -> TypeId) -> TypeKind {
    let id = |id: &TypeId| map(*id);
    let mut kind = kind.clone();
    match &mut kind {
        TypeKind::Primitive { .. } | TypeKind::Opaque { .. } => {}
        TypeKind::Pointer { pointee, .. } => *pointee = id(pointee),
        TypeKind::Array { element, .. } => *element = id(element),
        TypeKind::Struct { fields, .. } | TypeKind::Union { fields, .. } => {
            for field in fields {
                field.type_id = id(&field.type_id);
            }
        }
        TypeKind::Enum { underlying, .. } => *underlying = id(underlying),
        TypeKind::Function {
            parameters,
            return_type,
            ..
        } => {
            for parameter in parameters.iter_mut() {
                *parameter = id(parameter);
            }
            *return_type = id(return_type);
        }
        TypeKind::Typedef { target } => *target = id(target),
    }
    kind
}

pub(super) fn remap_function(function: &Function, map: impl Fn(TypeId) -> TypeId) -> Function {
    let mut remapped = function.clone();
    remapped.return_type = map(function.return_type);
    for parameter in &mut remapped.parameters {
        parameter.type_id = map(parameter.type_id);
    }
    remapped
}
