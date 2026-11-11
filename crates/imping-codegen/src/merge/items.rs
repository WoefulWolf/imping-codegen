//! Keys that match functions and globals across builds.

use imping_ir::{Global, Module, Rtti};
use std::collections::{HashMap, HashSet};

/// What identifies a function or global across builds: namespace, name, and
/// which of the items with those it is, in address order.
pub(super) type ItemKey = (Vec<String>, String, usize);

/// What identifies a global across builds: namespace, name, and either the
/// RTTI class and offset of the vtable it is, or which of the other globals
/// with those it is, in address order.
pub(super) type GlobalKey = (Vec<String>, String, Which);

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum Which {
    Vtable(String, u32),
    Index(usize),
}

/// Pairs a module's globals with their keys, in the globals' order.
pub(super) fn keyed_globals(module: &Module) -> Vec<(GlobalKey, &Global)> {
    let rtti: HashMap<u32, &Rtti> = module
        .vtables
        .iter()
        .filter_map(|vtable| Some((vtable.rva, vtable.rtti.as_ref()?)))
        .collect();
    let mut which: Vec<Option<Which>> = vec![None; module.globals.len()];
    let mut taken = HashSet::new();
    for (index, global) in module.globals.iter().enumerate() {
        if let Some(rtti) = rtti.get(&global.rva) {
            let key = (
                &global.namespace,
                &global.name,
                &rtti.type_name,
                rtti.offset,
            );
            // A class's vtable twice over is told apart by address instead.
            if taken.insert(key) {
                which[index] = Some(Which::Vtable(rtti.type_name.clone(), rtti.offset));
            }
        }
    }
    let mut order: Vec<usize> = (0..module.globals.len())
        .filter(|&index| which[index].is_none())
        .collect();
    order.sort_by_key(|&index| {
        let global = &module.globals[index];
        (&global.namespace, &global.name, global.rva)
    });
    let mut seen: HashMap<(&Vec<String>, &String), usize> = HashMap::new();
    for index in order {
        let global = &module.globals[index];
        let count = seen.entry((&global.namespace, &global.name)).or_default();
        which[index] = Some(Which::Index(*count));
        *count += 1;
    }
    module
        .globals
        .iter()
        .zip(which)
        .map(|(global, which)| {
            let which = which.expect("every global has a key");
            (
                (global.namespace.clone(), global.name.clone(), which),
                global,
            )
        })
        .collect()
}

/// Pairs items with their keys, in the items' order: items of the same
/// namespace and name are told apart by their order of address.
pub(super) fn keyed<'a, T>(
    items: &'a [T],
    identity: impl Fn(&'a T) -> (&'a Vec<String>, &'a String, u32),
) -> Vec<(ItemKey, &'a T)> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&index| identity(&items[index]));
    let mut seen: HashMap<(&Vec<String>, &String), usize> = HashMap::new();
    let mut keys: Vec<Option<ItemKey>> = vec![None; items.len()];
    for index in order {
        let (namespace, name, _) = identity(&items[index]);
        let count = seen.entry((namespace, name)).or_default();
        keys[index] = Some((namespace.clone(), name.clone(), *count));
        *count += 1;
    }
    keys.into_iter()
        .zip(items)
        .map(|(key, item)| (key.expect("every item has a key"), item))
        .collect()
}
