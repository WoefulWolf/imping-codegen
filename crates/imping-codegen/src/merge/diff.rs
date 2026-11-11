//! How two builds disagree, for `Error::BuildsDiffer`.

use super::types::kind_tag;
use crate::emit::qualified;
use crate::error::{BuildsDiffer, Error};
use imping_ir::{Function, Module, TypeDef, TypeId, TypeKind};

pub(super) fn differ(
    module: &Module,
    item: String,
    first: &str,
    second: &str,
    difference: String,
) -> Error {
    Error::BuildsDiffer(Box::new(BuildsDiffer {
        module: module.name.clone(),
        item,
        first: first.to_owned(),
        second: second.to_owned(),
        difference,
    }))
}

/// How two builds' signatures of a function differ, if they do.
pub(super) fn signature_difference(
    known: &Function,
    other: &Function,
    types: &[TypeDef],
) -> Option<String> {
    let describe = |id: TypeId| describe(types, id);
    if known.calling_convention != other.calling_convention {
        return Some(format!(
            "calling convention {:?} vs {:?}",
            known.calling_convention, other.calling_convention
        ));
    }
    if known.return_type != other.return_type {
        return Some(format!(
            "return type `{}` vs `{}`",
            describe(known.return_type),
            describe(other.return_type)
        ));
    }
    if known.parameters.len() != other.parameters.len() {
        return Some(format!(
            "{} parameters vs {}",
            known.parameters.len(),
            other.parameters.len()
        ));
    }
    for (index, (a, b)) in known.parameters.iter().zip(&other.parameters).enumerate() {
        let parameter = match &a.name {
            Some(name) => format!("parameter `{name}`"),
            None => format!("parameter {index}"),
        };
        if a.type_id != b.type_id {
            return Some(format!(
                "{parameter}: type `{}` vs `{}`",
                describe(a.type_id),
                describe(b.type_id)
            ));
        }
        if a.storage != b.storage {
            return Some(format!(
                "{parameter}: storage {:?} vs {:?}",
                a.storage, b.storage
            ));
        }
    }
    if known.variadic != other.variadic {
        return Some("variadic in one build only".to_owned());
    }
    if known.custom_storage != other.custom_storage {
        return Some("custom storage in one build only".to_owned());
    }
    None
}

/// How two builds' definitions of a type differ, if they do; both use merged
/// ids.
pub(super) fn kind_difference(
    known: &TypeKind,
    other: &TypeKind,
    types: &[TypeDef],
) -> Option<String> {
    if known == other {
        return None;
    }
    let name = |id: TypeId| format!("`{}`", describe(types, id));
    Some(match (known, other) {
        (
            TypeKind::Struct {
                size: a,
                align: x,
                fields: f,
            },
            TypeKind::Struct {
                size: b,
                align: y,
                fields: g,
            },
        )
        | (
            TypeKind::Union {
                size: a,
                align: x,
                fields: f,
            },
            TypeKind::Union {
                size: b,
                align: y,
                fields: g,
            },
        ) => {
            if a != b {
                format!("size {a:#x} vs {b:#x}")
            } else if x != y {
                format!("align {x} vs {y}")
            } else if f.len() != g.len() {
                format!("{} fields vs {}", f.len(), g.len())
            } else {
                let (index, (a, b)) = f
                    .iter()
                    .zip(g)
                    .enumerate()
                    .find(|(_, (a, b))| a != b)
                    .expect("the fields differ somewhere");
                if a.name != b.name {
                    format!("field {index}: `{}` vs `{}`", a.name, b.name)
                } else if a.offset != b.offset {
                    format!(
                        "field `{}`: offset {:#x} vs {:#x}",
                        a.name, a.offset, b.offset
                    )
                } else if a.type_id != b.type_id {
                    format!(
                        "field `{}`: type {} vs {}",
                        a.name,
                        name(a.type_id),
                        name(b.type_id)
                    )
                } else if a.base != b.base {
                    format!("field `{}`: a base class in one build only", a.name)
                } else {
                    format!("field `{}`: bits {:?} vs {:?}", a.name, a.bits, b.bits)
                }
            }
        }
        (
            TypeKind::Enum {
                underlying: a,
                variants: x,
            },
            TypeKind::Enum {
                underlying: b,
                variants: y,
            },
        ) => {
            if a != b {
                format!("underlying type {} vs {}", name(*a), name(*b))
            } else {
                match x.iter().zip(y).find(|(a, b)| a != b) {
                    Some((a, b)) => format!(
                        "constant `{}` = {} vs `{}` = {}",
                        a.name, a.value, b.name, b.value
                    ),
                    None => format!("{} constants vs {}", x.len(), y.len()),
                }
            }
        }
        (TypeKind::Typedef { target: a }, TypeKind::Typedef { target: b }) => {
            format!("target {} vs {}", name(*a), name(*b))
        }
        (TypeKind::Opaque { size: a }, TypeKind::Opaque { size: b }) => {
            format!("size {a:?} vs {b:?}")
        }
        (TypeKind::Primitive { primitive: a }, TypeKind::Primitive { primitive: b }) => {
            format!("{a:?} vs {b:?}")
        }
        // Pointer, array and function types match by shape, so never differ,
        // and named types only match types of their own kind.
        _ => format!("{} vs {}", kind_tag(known), kind_tag(other)),
    })
}

/// A type's name with its namespace, for messages.
pub(super) fn describe(types: &[TypeDef], id: TypeId) -> String {
    types.iter().find(|def| def.id == id).map_or_else(
        || format!("#{}", id.0),
        |def| qualified(&def.namespace, &def.name),
    )
}
