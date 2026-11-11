//! Merges the IR of several builds of a module into one module to generate
//! from, keeping each build's addresses beside it.
//!
//! Types, functions and globals are matched by namespace and name; items
//! with the same name are matched in address order, types in id order. A
//! global that is an RTTI vtable, such as a `vftable` label, is matched by its
//! class and offset instead, as the linker may reorder a class's vtables.
//! Pointer, array and function types have no identity of their own, so they
//! match by what they're built from. Matched items must agree on their types
//! and signatures, so the generated code fits every build.

mod diff;
mod items;
mod types;

use self::diff::{differ, signature_difference};
use self::items::{GlobalKey, ItemKey, keyed, keyed_globals};
use self::types::{Types, remap_function};
use crate::emit::qualified;
use crate::error::Error;
use crate::functions::{self, Slot};
use imping_ir::{Function, Global, Module};
use std::collections::HashMap;

/// One IR module merged across the builds that have it.
pub(crate) struct Merged<'a> {
    /// The module to generate from: every build's types, functions and
    /// globals, with the types of the first build that has each.
    pub(crate) module: Module,
    /// The labels of the builds that have the module, in the order given.
    pub(crate) labels: Vec<&'a str>,
    /// The builds `init()` accepts: an index into `labels`, the timestamp and
    /// the image size. Each label's own IR comes first, in label order.
    pub(crate) identities: Vec<(usize, u32, u32)>,
    /// Per function of `module`: where each build has it.
    pub(crate) functions: Vec<FunctionSites<'a>>,
    /// Per global of `module`: each build's label and its address there.
    pub(crate) globals: Vec<Vec<(&'a str, Option<u32>)>>,
}

/// Where each build has a function, and the vtable slot that finds it.
pub(crate) struct FunctionSites<'a> {
    /// Each build's label and its function, for its address, verification
    /// bytes, shape and pattern; `None` where its IR lacks it.
    pub(crate) sites: Vec<(&'a str, Option<&'a Function>)>,
    pub(crate) slot: Option<Slot<'a>>,
}

impl<'a> FunctionSites<'a> {
    /// The function in each build whose IR has it.
    pub(crate) fn present(&self) -> impl Iterator<Item = &'a Function> + '_ {
        self.sites.iter().filter_map(|(_, site)| *site)
    }
}

/// One entry per build, with its label, all `None`.
fn per_build<'a, T>(builds: &[(&'a str, &Module)]) -> Vec<(&'a str, Option<T>)> {
    builds.iter().map(|&(label, _)| (label, None)).collect()
}

/// Merges `builds`, the module in each build that has it, in label order.
/// `extra` are further accepted builds: label index, timestamp, image size.
pub(crate) fn merge<'a>(
    builds: &[(&'a str, &'a Module)],
    extra: &[(usize, u32, u32)],
) -> Result<Merged<'a>, Error> {
    let (first_label, first) = builds[0];
    let mut identities = Vec::new();
    for (index, (_, module)) in builds.iter().enumerate() {
        identities.push((index, module.timestamp, module.image_size));
    }
    for identity in extra {
        if !identities.contains(identity) {
            identities.push(*identity);
        }
    }
    for (label, module) in &builds[1..] {
        if module.architecture != first.architecture {
            return Err(differ(
                first,
                "the module".to_owned(),
                first_label,
                label,
                format!("{:?} vs {:?}", first.architecture, module.architecture),
            ));
        }
    }

    let mut types = Types::new(first);
    for &(label, module) in &builds[1..] {
        types.add(first, first_label, label, module)?;
    }

    let mut merged_functions: Vec<Function> = Vec::new();
    let mut function_sites: Vec<FunctionSites<'a>> = Vec::new();
    let mut function_index: HashMap<ItemKey, usize> = HashMap::new();
    let mut merged_globals: Vec<Global> = Vec::new();
    let mut global_rvas: Vec<Vec<(&'a str, Option<u32>)>> = Vec::new();
    let mut global_index: HashMap<GlobalKey, usize> = HashMap::new();
    let mut origin: Vec<&'a str> = Vec::new();

    for (build, &(label, module)) in builds.iter().enumerate() {
        let slots = functions::vtable_slots(module);
        let map = &types.maps[build];
        for (key, function) in keyed(&module.functions, |f| (&f.namespace, &f.name, f.rva)) {
            let remapped = remap_function(function, |id| map[&id]);
            match function_index.get(&key) {
                Some(&index) => {
                    if let Some(difference) = signature_difference(
                        &types.canonical_function(&merged_functions[index]),
                        &types.canonical_function(&remapped),
                        &types.merged,
                    ) {
                        return Err(differ(
                            first,
                            format!(
                                "function `{}`",
                                qualified(&function.namespace, &function.name)
                            ),
                            origin[index],
                            label,
                            difference,
                        ));
                    }
                    for tag in &function.tags {
                        if !merged_functions[index].tags.contains(tag) {
                            merged_functions[index].tags.push(tag.clone());
                        }
                    }
                    let entry = &mut function_sites[index];
                    entry.sites[build].1 = Some(function);
                    if entry.slot.is_none() {
                        entry.slot = functions::slot(&slots, function);
                    }
                }
                None => {
                    function_index.insert(key, merged_functions.len());
                    merged_functions.push(remapped);
                    origin.push(label);
                    let mut sites = per_build(builds);
                    sites[build].1 = Some(function);
                    function_sites.push(FunctionSites {
                        sites,
                        slot: functions::slot(&slots, function),
                    });
                }
            }
        }
    }

    let mut global_origin: Vec<&'a str> = Vec::new();
    for (build, &(label, module)) in builds.iter().enumerate() {
        let map = &types.maps[build];
        for (key, global) in keyed_globals(module) {
            let type_id = map[&global.type_id];
            match global_index.get(&key) {
                Some(&index) => {
                    let known = merged_globals[index].type_id;
                    if types.canonical(known) != types.canonical(type_id) {
                        return Err(differ(
                            first,
                            format!("global `{}`", qualified(&global.namespace, &global.name)),
                            global_origin[index],
                            label,
                            format!(
                                "type `{}` vs `{}`",
                                types.describe(known),
                                types.describe(type_id)
                            ),
                        ));
                    }
                    global_rvas[index][build].1 = Some(global.rva);
                }
                None => {
                    global_index.insert(key, merged_globals.len());
                    merged_globals.push(Global {
                        type_id,
                        ..global.clone()
                    });
                    global_origin.push(label);
                    let mut rvas = per_build(builds);
                    rvas[build].1 = Some(global.rva);
                    global_rvas.push(rvas);
                }
            }
        }
    }

    let module = Module {
        name: first.name.clone(),
        architecture: first.architecture,
        image_base: first.image_base,
        timestamp: first.timestamp,
        image_size: first.image_size,
        hash: None,
        types: types.merged,
        functions: merged_functions,
        globals: merged_globals,
        vtables: Vec::new(),
    };
    Ok(Merged {
        module,
        labels: builds.iter().map(|(label, _)| *label).collect(),
        identities,
        functions: function_sites,
        globals: global_rvas,
    })
}
