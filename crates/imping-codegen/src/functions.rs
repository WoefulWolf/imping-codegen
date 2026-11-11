//! Emits functions and globals, one module per namespace.
//!
//! Each function becomes an `unsafe fn` plus a module of the same name holding
//! its `RVA_<BUILD>` and `VERIFICATION_<BUILD>` per build, `rva()`,
//! `Signature`, `resolve()`, `ptr()` and `hook()`, and the private `FUNCTION`
//! they find it through. Types are named through explicit `super::` paths, not
//! glob imports, so an item in a namespace module (such as a constructor
//! `Entity::Entity`) can't shadow the type it refers to.

use crate::Warning;
use crate::emit::{doc, docs, hex, qualified};
use crate::layout::Layouts;
use crate::merge::{FunctionSites, Merged};
use crate::names::{self, Scope, build_const, doc_name, ident};
use crate::namespaces::Namespace;
use crate::types::{self, Text, TypeTable};
use imping_ir::{Function, Global, Module, Rtti, TypeId, TypeKind};
use proc_macro2::{Ident, TokenStream};
use quote::quote;
use std::collections::{HashMap, HashSet};

/// The most parameters a hookable function may have, the hooking engine's
/// limit.
const MAX_HOOK_PARAMETERS: usize = 14;

/// The items of a function's own module that its parameters could clash with
/// in Rust patterns, besides its per-build constants: its `FUNCTION` and
/// `SLOT` statics and the tuple struct `Original`.
const FUNCTION_MODULE_VALUES: &[&str] = &["FUNCTION", "SLOT", "Original"];

/// The function tag that marks a function as safe to call with valid
/// arguments.
///
/// Such a function has no other preconditions (any time, any thread) and keeps
/// none of its pointer arguments, so it gets a safe wrapper taking references.
pub(crate) const SAFE_TAG: &str = "imping-safe";

/// Adds a module's functions and globals to the namespace tree `root` and
/// returns the whole tree as tokens.
///
/// `root`'s modules must already be named and hold the types. In each scope,
/// functions claim names in RVA order (so the first overload keeps the plain
/// name), then globals.
pub(crate) fn emit(
    table: &TypeTable<'_>,
    layouts: &Layouts,
    mut root: Namespace,
    merged: &Merged<'_>,
    warnings: &mut Vec<Warning>,
) -> TokenStream {
    let module = table.module();
    let mut by_rva: Vec<usize> = (0..module.functions.len()).collect();
    by_rva.sort_by_key(|&index| (module.functions[index].rva, index));
    let mut function_names = vec![String::new(); module.functions.len()];
    for index in by_rva {
        let function = &module.functions[index];
        function_names[index] = root
            .at(&function.namespace)
            .scope
            .claim_numbered(&names::sanitize(&function.name));
    }
    let global_names: Vec<String> = module
        .globals
        .iter()
        .map(|global| {
            root.at(&global.namespace)
                .scope
                .claim_numbered(&names::sanitize(&global.name))
        })
        .collect();

    // Statics and tuple structs per namespace: a parameter with one of these
    // names would be read as a pattern.
    let mut values: HashMap<&[String], HashSet<&str>> = HashMap::new();
    values.entry(&[]).or_default().insert("MODULE");
    for (global, name) in module.globals.iter().zip(&global_names) {
        values
            .entry(global.namespace.as_slice())
            .or_default()
            .insert(name.as_str());
    }
    for (namespace, name) in table.tuple_struct_names() {
        values.entry(namespace).or_default().insert(name);
    }
    let no_values = HashSet::new();

    for ((function, name), sites) in module
        .functions
        .iter()
        .zip(&function_names)
        .zip(&merged.functions)
    {
        let taken = values
            .get(function.namespace.as_slice())
            .unwrap_or(&no_values);
        let item = emit_function(table, layouts, function, name, taken, sites, warnings);
        root.at(&function.namespace).items.push(item);
    }
    for ((global, name), rvas) in module
        .globals
        .iter()
        .zip(&global_names)
        .zip(&merged.globals)
    {
        let item = emit_global(table, global, name, rvas);
        root.at(&global.namespace).items.push(item);
    }
    root.into_tokens()
}

/// Lists an item's addresses for its docs: `` `game.exe+0x15040` `` with one
/// build, otherwise each build's, and the builds whose IR lacks it.
fn addresses(file: &str, rvas: &[(&str, Option<u32>)]) -> String {
    if let [(_, Some(rva))] = rvas {
        return format!("`{file}+{rva:#x}`");
    }
    let found: Vec<String> = rvas
        .iter()
        .filter_map(|(label, rva)| rva.map(|rva| format!("`{file}+{rva:#x}` in `{label}`")))
        .collect();
    let missing: Vec<String> = rvas
        .iter()
        .filter(|(_, rva)| rva.is_none())
        .map(|(label, _)| format!("`{label}`"))
        .collect();
    let mut text = found.join(", ");
    if !missing.is_empty() {
        text.push_str(&format!(" (not in the IR of {})", missing.join(", ")));
    }
    text
}

/// Returns `super::` repeated `depth` times, the path from a module `depth`
/// levels below the bindings' root back to it.
fn supers(depth: usize) -> TokenStream {
    let supers = (0..depth).map(|_| quote! { super:: });
    quote! { #(#supers)* }
}

/// A parameter: its name, and its type as written in the namespace module and
/// one level below, in the function's own module.
struct Parameter {
    name: Ident,
    outer: TokenStream,
    inner: TokenStream,
    /// The IR type, and the IR name for messages.
    type_id: TypeId,
    source: String,
}

/// Returns whether a type can be passed or returned by value.
///
/// Arrays and function types decay to pointers, as in C.
fn by_value(table: &TypeTable<'_>, layouts: &Layouts, id: TypeId) -> bool {
    match table.peel(id).map(|def| &def.kind) {
        Some(TypeKind::Array { .. } | TypeKind::Function { .. }) => true,
        _ => layouts.layout(id).is_some(),
    }
}

/// Returns a parameter or return type, with arrays decayed to pointers (text
/// arrays to the runtime's string pointers).
fn value_type(table: &TypeTable<'_>, id: TypeId, depth: usize) -> TokenStream {
    match table.peel(id).map(|def| &def.kind) {
        Some(TypeKind::Array { element, .. }) => match table.text_kind(*element) {
            Some(text) => text.pointer(),
            None => {
                let element = table.rust_type(*element, depth);
                quote! { *mut #element }
            }
        },
        _ => table.rust_type(id, depth),
    }
}

/// A parameter of a safe call wrapper: its declaration, and the expression
/// that passes it to the raw function.
struct SafeParameter {
    declaration: TokenStream,
    argument: TokenStream,
}

/// Returns the safe form of a parameter of a function tagged [`SAFE_TAG`], or
/// why it has none.
///
/// Data pointers become references (text pointers `&CStr` or `&WString`).
/// By-value parameters must hold no data pointers, since safe code could put
/// any address in them.
fn safe_parameter(
    table: &TypeTable<'_>,
    layouts: &Layouts,
    parameter: &Parameter,
    depth: usize,
) -> Result<SafeParameter, String> {
    let Parameter {
        name,
        outer,
        type_id,
        source,
        ..
    } = parameter;
    let unchanged = || SafeParameter {
        declaration: quote! { #name: #outer },
        argument: quote! { #name },
    };
    let text = |text: Text| match text {
        Text::Narrow => SafeParameter {
            declaration: quote! { #name: &::core::ffi::CStr },
            argument: quote! { ::imping_runtime::CStrPtr::new(#name.as_ptr()) },
        },
        Text::Wide => SafeParameter {
            declaration: quote! { #name: &::imping_runtime::WString },
            argument: quote! { #name.as_ptr() },
        },
    };
    let def = table
        .peel(*type_id)
        .ok_or_else(|| format!("parameter `{source}` is part of a typedef cycle"))?;
    match &def.kind {
        TypeKind::Pointer { pointee, is_const } => {
            if table
                .peel(*pointee)
                .is_some_and(|def| matches!(def.kind, TypeKind::Function { .. }))
            {
                return Ok(unchanged());
            }
            if let Some(kind) = table.text_kind(*pointee) {
                return Ok(text(kind));
            }
            if table.is_void(*pointee) {
                return Err(format!("parameter `{source}` is a `void` pointer"));
            }
            if layouts.layout(*pointee).is_none() {
                return Err(format!(
                    "parameter `{source}` points to a type of unknown size"
                ));
            }
            let target = table.rust_type(*pointee, depth);
            Ok(if *is_const {
                SafeParameter {
                    declaration: quote! { #name: &#target },
                    argument: quote! { ::core::ptr::from_ref(#name) },
                }
            } else {
                SafeParameter {
                    declaration: quote! { #name: &mut #target },
                    argument: quote! { ::core::ptr::from_mut(#name) },
                }
            })
        }
        TypeKind::Array { element, .. } => match table.text_kind(*element) {
            Some(kind) => Ok(text(kind)),
            None => Err(format!("parameter `{source}` is an array")),
        },
        _ if table.is_pointer_free(*type_id) => Ok(unchanged()),
        _ => Err(format!(
            "parameter `{source}` is passed by value and contains pointers"
        )),
    }
}

/// Returns the function's parameters, or why one can't be passed.
///
/// A `void` parameter means "no parameters", as in C, and is dropped. A name
/// that clashes with a static or tuple struct in scope (`taken`, or
/// [`FUNCTION_MODULE_VALUES`]) gets a trailing `_`, so it isn't read as a
/// pattern.
fn parameters(
    table: &TypeTable<'_>,
    layouts: &Layouts,
    function: &Function,
    depth: usize,
    taken: &HashSet<&str>,
) -> Result<Vec<Parameter>, String> {
    let mut scope = Scope::default();
    // Locals of the generated thunk.
    scope.reserve("detour");
    scope.reserve("original");
    let mut parameters = Vec::with_capacity(function.parameters.len());
    for (index, parameter) in function.parameters.iter().enumerate() {
        if table.is_void(parameter.type_id) {
            continue;
        }
        let source_name = parameter
            .name
            .clone()
            .unwrap_or_else(|| format!("arg{index}"));
        if !by_value(table, layouts, parameter.type_id) {
            return Err(format!("parameter `{source_name}` has no known size"));
        }
        let mut name = names::sanitize(&source_name);
        let per_build = name.starts_with("RVA_") || name.starts_with("VERIFICATION_");
        if taken.contains(name.as_str())
            || FUNCTION_MODULE_VALUES.contains(&name.as_str())
            || per_build
        {
            name.push('_');
        }
        parameters.push(Parameter {
            name: ident(&scope.claim_numbered(&name)),
            outer: value_type(table, parameter.type_id, depth),
            inner: value_type(table, parameter.type_id, depth + 1),
            type_id: parameter.type_id,
            source: source_name,
        });
    }
    Ok(parameters)
}

/// A vtable slot a function is in: its vtable's namespace and RVA, the class's
/// RTTI, and the slot's index.
pub(crate) type Slot<'a> = (&'a [String], u32, &'a Rtti, u32);

/// Returns the RTTI vtable slots each function address is in.
pub(crate) fn vtable_slots(module: &Module) -> HashMap<u32, Vec<Slot<'_>>> {
    let mut slots: HashMap<u32, Vec<Slot<'_>>> = HashMap::new();
    for vtable in &module.vtables {
        let Some(rtti) = &vtable.rtti else {
            continue;
        };
        for entry in &vtable.entries {
            slots.entry(entry.rva).or_default().push((
                vtable.namespace.as_slice(),
                vtable.rva,
                rtti,
                entry.index,
            ));
        }
    }
    slots
}

/// Picks the slot a function is found through when its RVA and pattern fail:
/// in its own class's vtable if it's there, otherwise the first vtable holding
/// it. Only for functions with a shape, which checks the slot still holds it.
pub(crate) fn slot<'a>(
    slots: &HashMap<u32, Vec<Slot<'a>>>,
    function: &Function,
) -> Option<Slot<'a>> {
    if function.shape.is_empty() {
        return None;
    }
    let candidates = slots.get(&function.rva)?;
    candidates
        .iter()
        .find(|(namespace, ..)| *namespace == function.namespace.as_slice())
        .or_else(|| candidates.iter().min_by_key(|(_, rva, ..)| *rva))
        .copied()
}

/// Emits one function: the call wrapper and its module, or as much of them as
/// the function allows.
fn emit_function(
    table: &TypeTable<'_>,
    layouts: &Layouts,
    function: &Function,
    name: &str,
    taken: &HashSet<&str>,
    sites: &FunctionSites<'_>,
    warnings: &mut Vec<Warning>,
) -> TokenStream {
    let module = table.module();
    let depth = function.namespace.len();
    let qualified_name = qualified(&function.namespace, &function.name);
    let shown = doc_name(&qualified_name);
    let file = doc_name(&module.name);
    let fn_ident = ident(name);
    let rvas: Vec<(&str, Option<u32>)> = sites
        .sites
        .iter()
        .map(|&(label, site)| (label, site.map(|function| function.rva)))
        .collect();
    let mut summary = format!("`{shown}` at {}.", addresses(&file, &rvas));
    if !function.tags.is_empty() {
        let tags: Vec<String> = function
            .tags
            .iter()
            .map(|tag| format!("`{}`", doc_name(tag)))
            .collect();
        summary.push_str(&format!(" Tags: {}.", tags.join(", ")));
    }
    let rva_items = sites.sites.iter().filter_map(|&(label, site)| {
        let site = site?;
        let doc = doc(format!(
            "The function's address relative to `{file}`'s base in the `{label}` build."
        ));
        let name = ident(&format!("RVA_{}", build_const(label)));
        let rva = hex(u64::from(site.rva));
        Some(quote! {
            #doc
            pub const #name: u32 = #rva;
        })
    });
    let rva_items = quote! { #(#rva_items)* };

    if function.custom_storage {
        warnings.push(Warning::CustomStorage {
            module: module.name.clone(),
            function: qualified_name,
        });
        let doc = docs(&[
            summary,
            "It uses custom argument storage, so only its addresses are generated.".to_owned(),
        ]);
        return quote! {
            #doc
            pub mod #fn_ident {
                #rva_items
            }
        };
    }

    let skip = |reason: String, warnings: &mut Vec<Warning>| {
        warnings.push(Warning::FunctionSkipped {
            module: module.name.clone(),
            function: qualified_name.clone(),
            reason,
        });
        TokenStream::new()
    };
    let parameters = match parameters(table, layouts, function, depth, taken) {
        Ok(parameters) => parameters,
        Err(reason) => return skip(reason, warnings),
    };
    let (output_outer, output_inner) = if table.is_void(function.return_type) {
        (None, None)
    } else if by_value(table, layouts, function.return_type) {
        let outer = value_type(table, function.return_type, depth);
        let inner = value_type(table, function.return_type, depth + 1);
        (Some(quote! { -> #outer }), Some(quote! { -> #inner }))
    } else {
        return skip("its return type has no known size".to_owned(), warnings);
    };

    let abi = types::abi(
        module.architecture,
        function.calling_convention,
        function.variadic,
    );
    let to_root = supers(depth + 1);
    let signature_params: Vec<TokenStream> = parameters
        .iter()
        .map(|Parameter { name, inner, .. }| quote! { #name: #inner })
        .collect();
    let dots = function.variadic.then(|| quote! { , ... });
    let signature_item = quote! {
        #[doc = " The function's type."]
        pub type Signature = unsafe extern #abi fn(#(#signature_params),* #dots) #output_inner;
    };
    let verified = sites.present().any(|site| !site.verification.is_empty());
    let verification_items = sites.sites.iter().filter_map(|&(label, site)| {
        let site = site.filter(|site| !site.verification.is_empty())?;
        let doc = doc(format!(
            "The bytes the function starts with in the `{label}` build; `resolve` and `hook` \
             check them."
        ));
        let name = ident(&format!("VERIFICATION_{}", build_const(label)));
        let bytes = site.verification.iter().map(|byte| hex(u64::from(*byte)));
        Some(quote! {
            #doc
            pub const #name: &[u8] = &[#(#bytes),*];
        })
    });
    let verification_items = quote! { #(#verification_items)* };
    let function_item = function_item(&qualified_name, sites, &to_root);
    let has_pattern = sites.present().any(|site| site.pattern.is_some());
    let fallback = match (has_pattern, sites.slot.is_some()) {
        (true, true) => Some("by its byte pattern or its class's vtable"),
        (true, false) => Some("by its byte pattern"),
        (false, true) => Some("through its class's vtable"),
        (false, false) => None,
    };
    let missing = sites.sites.iter().any(|(_, site)| site.is_none());
    let resolve_item = resolve_item(&file, verified, fallback, missing);
    let rva_fn_doc = docs(&[format!(
        "The function's address relative to `{file}`'s base in the loaded build: `None` before \
         `init()`, on a build none of the IRs is, or if the loaded build's IR lacks it."
    )]);
    let rva_fn = quote! {
        #rva_fn_doc
        pub fn rva() -> ::core::option::Option<u32> {
            FUNCTION.rva()
        }
    };
    let ptr_doc = docs(&[
        "The function's address in this process.".to_owned(),
        "# Panics".to_owned(),
        format!("If `resolve()` fails, as it does before `{file}`'s `init()`."),
    ]);
    let ptr_item = quote! {
        #ptr_doc
        pub fn ptr() -> Signature {
            match resolve() {
                ::core::result::Result::Ok(function) => function,
                ::core::result::Result::Err(error) => ::core::panic!("imping: {error}"),
            }
        }
    };

    if function.variadic {
        let doc = docs(&[
            summary,
            format!("Variadic: call it through `{name}::ptr()`; it can't be hooked."),
        ]);
        return quote! {
            #doc
            pub mod #fn_ident {
                #rva_items
                #verification_items
                #rva_fn
                #signature_item
                #function_item
                #resolve_item
                #ptr_item
            }
        };
    }

    let hook_items = if parameters.len() <= MAX_HOOK_PARAMETERS {
        hook_items(
            &parameters,
            &output_inner,
            &abi,
            &shown,
            &file,
            verified,
            fallback,
        )
    } else {
        warnings.push(Warning::HookUnavailable {
            module: module.name.clone(),
            function: qualified_name.clone(),
            reason: format!(
                "it has {} parameters, more than the {MAX_HOOK_PARAMETERS} the hooking engine supports",
                parameters.len()
            ),
        });
        TokenStream::new()
    };

    let safe = if function.tags.iter().any(|tag| tag == SAFE_TAG) {
        match parameters
            .iter()
            .map(|parameter| safe_parameter(table, layouts, parameter, depth))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(safe) => Some(safe),
            Err(reason) => {
                warnings.push(Warning::SafeCallUnavailable {
                    module: module.name.clone(),
                    function: qualified_name.clone(),
                    reason,
                });
                None
            }
        }
    } else {
        None
    };

    let mut wrapper_paragraphs = vec![summary];
    if parameters.len() > MAX_HOOK_PARAMETERS {
        wrapper_paragraphs.push(format!(
            "It has more than {MAX_HOOK_PARAMETERS} parameters, so it can't be hooked."
        ));
    }
    let panics = [
        "# Panics".to_owned(),
        format!("If `{name}::resolve()` fails, as it does before `{file}`'s `init()`."),
    ];
    let wrapper = match safe {
        Some(safe) => {
            wrapper_paragraphs.push(format!(
                "Tagged `{SAFE_TAG}`, so it is safe to call: valid arguments are all it needs, \
                 and its pointer parameters are references."
            ));
            wrapper_paragraphs.extend(panics);
            let wrapper_doc = docs(&wrapper_paragraphs);
            let declarations = safe.iter().map(|parameter| &parameter.declaration);
            let arguments = safe.iter().map(|parameter| &parameter.argument);
            quote! {
                #wrapper_doc
                pub fn #fn_ident(#(#declarations),*) #output_outer {
                    #[comment = " SAFETY: the function is tagged `imping-safe` in the IR, which `init`'s caller vouched for: it needs only valid arguments and keeps none of the pointers, and the parameter types guarantee valid arguments."]
                    unsafe { #fn_ident::ptr()(#(#arguments),*) }
                }
            }
        }
        None => {
            wrapper_paragraphs.extend([
                "# Safety".to_owned(),
                format!(
                    "Calls into `{file}`: its `init()` must have succeeded, and the arguments \
                     must be valid for this function."
                ),
            ]);
            wrapper_paragraphs.extend(panics);
            let wrapper_doc = docs(&wrapper_paragraphs);
            let wrapper_params = parameters
                .iter()
                .map(|Parameter { name, outer, .. }| quote! { #name: #outer });
            let args = parameters.iter().map(|parameter| &parameter.name);
            quote! {
                #wrapper_doc
                pub unsafe fn #fn_ident(#(#wrapper_params),*) #output_outer {
                    #[comment = " SAFETY: forwarded to the caller."]
                    unsafe { #fn_ident::ptr()(#(#args),*) }
                }
            }
        }
    };
    let module_doc = doc(format!("Address, signature and hook of `{shown}`."));
    quote! {
        #wrapper

        #module_doc
        pub mod #fn_ident {
            #rva_items
            #verification_items
            #rva_fn
            #signature_item
            #function_item
            #resolve_item
            #ptr_item
            #hook_items
        }
    }
}

/// Returns the private `FUNCTION` static that `resolve()` and `hook()` find the
/// function through, with a site per build.
fn function_item(
    qualified_name: &str,
    sites: &FunctionSites<'_>,
    to_root: &TokenStream,
) -> TokenStream {
    let sites_tokens = sites.sites.iter().map(|&(label, site)| {
        let Some(function) = site else {
            return quote! { ::core::option::Option::None };
        };
        let suffix = build_const(label);
        let rva = ident(&format!("RVA_{suffix}"));
        let verification = if function.verification.is_empty() {
            quote! { &[] }
        } else {
            let name = ident(&format!("VERIFICATION_{suffix}"));
            quote! { #name }
        };
        let pattern = match &function.pattern {
            Some(pattern) => {
                let bytes = &pattern.bytes;
                let offset = proc_macro2::Literal::i32_unsuffixed(pattern.offset);
                let follow_call = pattern.follow_call;
                quote! {
                    ::core::option::Option::Some(::imping_runtime::__private::Pattern {
                        bytes: #bytes,
                        offset: #offset,
                        follow_call: #follow_call,
                    })
                }
            }
            None => quote! { ::core::option::Option::None },
        };
        let shape = &function.shape;
        quote! {
            ::core::option::Option::Some(::imping_runtime::__private::Site {
                rva: #rva,
                verification: #verification,
                shape: #shape,
                pattern: #pattern,
            })
        }
    });
    let slot = match sites.slot {
        Some((_, _, rtti, index)) => {
            let type_name = &rtti.type_name;
            let offset = proc_macro2::Literal::u32_unsuffixed(rtti.offset);
            let index = proc_macro2::Literal::u32_unsuffixed(index);
            quote! {
                ::core::option::Option::Some(::imping_runtime::__private::VtableSlot {
                    type_name: #type_name,
                    offset: #offset,
                    index: #index,
                })
            }
        }
        None => quote! { ::core::option::Option::None },
    };
    quote! {
        static FUNCTION: ::imping_runtime::__private::Function =
            ::imping_runtime::__private::Function::new(
                &#to_root MODULE,
                #qualified_name,
                &[#(#sites_tokens),*],
                #slot,
            );
    }
}

/// Returns a function's `resolve()`. `missing` says some build's IR lacks the
/// function.
fn resolve_item(file: &str, verified: bool, fallback: Option<&str>, missing: bool) -> TokenStream {
    let (mut how, mut errors) = if verified {
        let mut how = format!(
            "On a build `{file}`'s `init()` accepts, that's the function's RVA in that build. On \
             another, the first call checks the bytes at each build's RVA against that build's \
             `VERIFICATION_` bytes"
        );
        let mut errors = format!(
            "Fails unless `{file}`'s `init()` has succeeded, or on a build it doesn't accept, \
             if no build's bytes match"
        );
        if let Some(fallback) = fallback {
            how.push_str(&format!(
                ", and if none match, finds the function {fallback} instead"
            ));
            errors.push_str(&format!(" and it can't be found {fallback}"));
        }
        how.push('.');
        (how, errors)
    } else {
        (
            "That's the function's RVA in the loaded build, unchecked, as there are no bytes to \
             verify it by."
                .to_owned(),
            format!("Fails unless `{file}`'s `init()` has succeeded"),
        )
    };
    if missing {
        match fallback {
            Some(fallback) => how.push_str(&format!(
                " Where the loaded build's IR lacks it, the first call finds it {fallback}."
            )),
            None => how.push_str(" Where the loaded build's IR lacks it, nothing finds it."),
        }
        errors.push_str(", or where the loaded build's IR lacks it, if nothing finds it");
    }
    errors.push('.');
    let doc = docs(&[
        "Finds the function in this process.".to_owned(),
        how,
        "# Errors".to_owned(),
        errors,
    ]);
    quote! {
        #doc
        pub fn resolve() -> ::imping_runtime::Result<Signature> {
            let address = FUNCTION.resolve()?;
            #[comment = " SAFETY: `resolve` returns the function's non-null address, and `init`'s caller promised the function has this type."]
            ::core::result::Result::Ok(unsafe { ::core::mem::transmute::<usize, Signature>(address) })
        }
    }
}

/// Returns the closure slot, thunk and `hook()` of a hookable function, for
/// its module.
fn hook_items(
    parameters: &[Parameter],
    output: &Option<TokenStream>,
    abi: &syn::LitStr,
    shown: &str,
    file: &str,
    verified: bool,
    fallback: Option<&str>,
) -> TokenStream {
    let types: Vec<&TokenStream> = parameters
        .iter()
        .map(|parameter| &parameter.inner)
        .collect();
    let thunk_params: Vec<TokenStream> = parameters
        .iter()
        .map(|Parameter { name, inner, .. }| quote! { #name: #inner })
        .collect();
    let args: Vec<&Ident> = parameters.iter().map(|parameter| &parameter.name).collect();
    let bound = quote! { for<'call> Fn(Original<'call>, #(#types),*) #output };
    let mut errors = format!(
        "Fails unless `{file}`'s `init()` has succeeded, if this mod already holds a hook on \
         the function"
    );
    if verified {
        errors
            .push_str(", if its first bytes differ from the loaded build's `VERIFICATION_` bytes");
        if let Some(fallback) = fallback {
            errors.push_str(&format!(" and it can't be found {fallback}"));
        }
    }
    errors.push_str(
        ", if the loaded build's IR lacks it and nothing finds it, or if patching fails.",
    );
    let hook_doc = docs(&[
        format!(
            "Hooks `{shown}`: once enabled, every call runs `detour`, which receives an \
             `Original` first, then the arguments. `original.forward()` calls the original \
             function with the arguments the target passed; `original.call(..)` calls it with \
             other ones."
        ),
        "The hook is returned disabled; enable it with `Hook::enable`. Dropping the guard \
         disables it, after which the function can be hooked again. A panic inside `detour` \
         aborts the process."
            .to_owned(),
        "# Errors".to_owned(),
        errors,
    ]);
    let original_doc = docs(&[format!(
        "The original `{shown}`, handed to a hook's `detour` with the arguments of the call \
             being hooked. It can only be used during that call."
    )]);
    let call_doc = docs(&[
        "Calls the original function with other arguments.".to_owned(),
        "# Safety".to_owned(),
        "The arguments must be valid for this function.".to_owned(),
    ]);
    quote! {
        #original_doc
        pub struct Original<'call>(Signature, (#(#types,)*), ::core::marker::PhantomData<&'call ()>);

        impl Original<'_> {
            /// Calls the original function with the arguments the target passed to the hooked call.
            pub fn forward(self) #output {
                let (#(#args,)*) = self.1;
                #[comment = " SAFETY: these are the arguments the target passed to the hooked call, so this is the call it was making; `init`'s caller promised the function has type `Signature`."]
                unsafe { (self.0)(#(#args),*) }
            }

            #call_doc
            pub unsafe fn call(self, #(#thunk_params),*) #output {
                #[comment = " SAFETY: forwarded to the caller."]
                unsafe { (self.0)(#(#args),*) }
            }
        }

        static SLOT: ::imping_runtime::__private::Slot<dyn #bound + Send + Sync, Signature> =
            ::imping_runtime::__private::Slot::new();

        unsafe extern #abi fn thunk(#(#thunk_params),*) #output {
            let (detour, original) = SLOT.get();
            detour(
                Original(original, (#(#args,)*), ::core::marker::PhantomData),
                #(#args),*
            )
        }

        #hook_doc
        pub fn hook(
            detour: impl #bound + Send + Sync + 'static,
        ) -> ::imping_runtime::Result<::imping_runtime::Hook<Signature>> {
            #[comment = " SAFETY: `thunk` has type `Signature` and calls the slot's closure; `init`'s caller promised the function has that type, and `install` fails unless `init` succeeded."]
            unsafe {
                ::imping_runtime::__private::install(
                    &FUNCTION,
                    &SLOT,
                    ::std::boxed::Box::new(detour),
                    thunk,
                )
            }
        }
    }
}

/// Emits one global as a `static` `imping_runtime::Global`, with its address
/// in each build.
fn emit_global(
    table: &TypeTable<'_>,
    global: &Global,
    name: &str,
    rvas: &[(&str, Option<u32>)],
) -> TokenStream {
    let module = table.module();
    let depth = global.namespace.len();
    let qualified_name = qualified(&global.namespace, &global.name);
    let shown = doc_name(&qualified_name);
    let file = doc_name(&module.name);
    let doc = doc(format!("`{shown}` at {}.", addresses(&file, rvas)));
    let name = ident(name);
    let ty = table.rust_type(global.type_id, depth);
    let to_root = supers(depth);
    let rvas = rvas.iter().map(|(_, rva)| match rva {
        Some(rva) => {
            let rva = hex(u64::from(*rva));
            quote! { ::core::option::Option::Some(#rva) }
        }
        None => quote! { ::core::option::Option::None },
    });
    quote! {
        #doc
        pub static #name: ::imping_runtime::Global<#ty> =
            ::imping_runtime::__private::global(&#to_root MODULE, #qualified_name, &[#(#rvas),*]);
    }
}
