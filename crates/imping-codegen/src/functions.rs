//! Emitting functions and globals, grouped into one module per namespace.
//!
//! Each function becomes an `unsafe fn` plus a module of the same name holding
//! its `RVA`, `VERIFICATION`, `Signature`, `ptr()` and `hook()`. Types are
//! named through explicit `super::` paths rather than glob imports, so an item
//! in a namespace module (such as a constructor `Entity::Entity`) can never
//! shadow the type it refers to.

use crate::Warning;
use crate::emit::{doc, docs, hex, qualified};
use crate::layout::Layouts;
use crate::names::{self, Scope, doc_name, ident};
use crate::types::{self, Text, TypeTable};
use imping_ir::{Function, Global, TypeId, TypeKind};
use proc_macro2::{Ident, TokenStream};
use quote::quote;
use std::collections::HashMap;

/// The most parameters a hookable function may have; the hooking engine
/// supports up to 14.
const MAX_HOOK_PARAMETERS: usize = 14;

/// The function tag that marks a function as safe to call with valid
/// arguments: it has no other preconditions (any time, any thread) and does
/// not keep its pointer arguments after returning. Such functions get a safe
/// call wrapper whose pointer parameters are references.
pub(crate) const SAFE_TAG: &str = "imping-safe";

/// The functions and globals of one namespace, and its child namespaces.
struct Namespace {
    ir_name: String,
    rust_name: String,
    scope: Scope,
    items: Vec<TokenStream>,
    children: Vec<Namespace>,
    index: HashMap<String, usize>,
}

impl Namespace {
    fn new(ir_name: String, rust_name: String, scope: Scope) -> Self {
        Self {
            ir_name,
            rust_name,
            scope,
            items: Vec::new(),
            children: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// The namespace at `path` below this one, created if needed. A new
    /// namespace's module name is claimed in its parent's scope.
    fn at(&mut self, path: &[String]) -> &mut Namespace {
        let Some((first, rest)) = path.split_first() else {
            return self;
        };
        let index = match self.index.get(first) {
            Some(&index) => index,
            None => {
                let rust_name = self.scope.claim_numbered(&names::snake_case(first));
                self.children
                    .push(Namespace::new(first.clone(), rust_name, Scope::default()));
                self.index.insert(first.clone(), self.children.len() - 1);
                self.children.len() - 1
            }
        };
        self.children[index].at(rest)
    }

    /// The namespace as a `pub mod`, below the namespace `parent`.
    fn into_module(self, parent: &[String]) -> TokenStream {
        let mut path = parent.to_vec();
        path.push(self.ir_name);
        let doc = doc(format!(
            "Functions and globals in the `{}` namespace.",
            doc_name(&path.join("::"))
        ));
        let name = ident(&self.rust_name);
        let items = self.items;
        let children = self
            .children
            .into_iter()
            .map(|child| child.into_module(&path));
        quote! {
            #doc
            pub mod #name {
                #(#items)*
                #(#children)*
            }
        }
    }
}

/// Emits a module's functions and globals: those without a namespace first,
/// then one `pub mod` per top-level namespace.
///
/// `scope` is the module root's scope, in which the types already have their
/// names. Within each scope, namespace modules claim names first, then
/// functions in RVA order (so the first of an overload set keeps the plain
/// name), then globals.
pub(crate) fn emit(
    table: &TypeTable<'_>,
    layouts: &Layouts,
    scope: Scope,
    warnings: &mut Vec<Warning>,
) -> TokenStream {
    let module = table.module();
    let mut root = Namespace::new(String::new(), String::new(), scope);
    for function in &module.functions {
        root.at(&function.namespace);
    }
    for global in &module.globals {
        root.at(&global.namespace);
    }

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

    for (function, name) in module.functions.iter().zip(&function_names) {
        let item = emit_function(table, layouts, function, name, warnings);
        root.at(&function.namespace).items.push(item);
    }
    for (global, name) in module.globals.iter().zip(&global_names) {
        let item = emit_global(table, global, name);
        root.at(&global.namespace).items.push(item);
    }

    let items = root.items;
    let children = root
        .children
        .into_iter()
        .map(|child| child.into_module(&[]));
    quote! {
        #(#items)*
        #(#children)*
    }
}

/// `super::` repeated `depth` times: the path from a module `depth` levels
/// below the bindings' module root back to it.
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

/// Whether a type can be passed or returned by value. Arrays and function
/// types decay to pointers, as in C.
fn by_value(table: &TypeTable<'_>, layouts: &Layouts, id: TypeId) -> bool {
    match table.peel(id).map(|def| &def.kind) {
        Some(TypeKind::Array { .. } | TypeKind::Function { .. }) => true,
        _ => layouts.layout(id).is_some(),
    }
}

/// A parameter or return type, with arrays decayed to pointers (text arrays
/// to the runtime's string pointers).
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

/// The safe form of a parameter of a function tagged [`SAFE_TAG`], or why it
/// has none. Data pointers become references (text pointers `&CStr` or
/// `&WString`); by-value parameters must contain no data pointers, since safe
/// code could put any address in them.
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

/// The function's parameters, or why one can't be passed. A `void`
/// parameter means "no parameters", as in C, and is dropped.
fn parameters(
    table: &TypeTable<'_>,
    layouts: &Layouts,
    function: &Function,
    depth: usize,
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
        parameters.push(Parameter {
            name: ident(&scope.claim_numbered(&names::sanitize(&source_name))),
            outer: value_type(table, parameter.type_id, depth),
            inner: value_type(table, parameter.type_id, depth + 1),
            type_id: parameter.type_id,
            source: source_name,
        });
    }
    Ok(parameters)
}

/// Emits one function: the call wrapper and its module, or as much of them as
/// the function allows.
fn emit_function(
    table: &TypeTable<'_>,
    layouts: &Layouts,
    function: &Function,
    name: &str,
    warnings: &mut Vec<Warning>,
) -> TokenStream {
    let module = table.module();
    let depth = function.namespace.len();
    let qualified_name = qualified(&function.namespace, &function.name);
    let shown = doc_name(&qualified_name);
    let file = doc_name(&module.name);
    let fn_ident = ident(name);
    let mut summary = format!("`{shown}` at `{file}+{:#x}`.", function.rva);
    if !function.tags.is_empty() {
        let tags: Vec<String> = function
            .tags
            .iter()
            .map(|tag| format!("`{}`", doc_name(tag)))
            .collect();
        summary.push_str(&format!(" Tags: {}.", tags.join(", ")));
    }
    let rva_doc = doc(format!(
        "The function's address relative to `{file}`'s base."
    ));
    let rva = hex(u64::from(function.rva));
    let rva_item = quote! {
        #rva_doc
        pub const RVA: u32 = #rva;
    };

    if function.custom_storage {
        warnings.push(Warning::CustomStorage {
            module: module.name.clone(),
            function: qualified_name,
        });
        let doc = docs(&[
            summary,
            "It uses custom argument storage, so only its address is generated.".to_owned(),
        ]);
        return quote! {
            #doc
            pub mod #fn_ident {
                #rva_item
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
    let parameters = match parameters(table, layouts, function, depth) {
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
    let ptr_doc = docs(&[
        "The function's address in this process.".to_owned(),
        "# Panics".to_owned(),
        format!("If `{file}`'s `init()` has not succeeded."),
    ]);
    let ptr_item = quote! {
        #ptr_doc
        pub fn ptr() -> Signature {
            #[comment = " SAFETY: `address` panics unless `init` succeeded, so this is the function's non-null address, and `init`'s caller promised the function has this type."]
            unsafe { ::core::mem::transmute::<usize, Signature>(#to_root MODULE.address(RVA)) }
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
                #rva_item
                #signature_item
                #ptr_item
            }
        };
    }

    let verification_item = (!function.verification.is_empty()).then(|| {
        let bytes = function
            .verification
            .iter()
            .map(|byte| hex(u64::from(*byte)));
        quote! {
            #[doc = " The bytes expected at the function's start; `hook` checks them before patching it."]
            pub const VERIFICATION: &[u8] = &[#(#bytes),*];
        }
    });
    let hook_items = if parameters.len() <= MAX_HOOK_PARAMETERS {
        hook_items(
            &parameters,
            &output_inner,
            &abi,
            &shown,
            &file,
            &to_root,
            !function.verification.is_empty(),
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
        format!("If `{file}`'s `init()` has not succeeded."),
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
            #rva_item
            #verification_item
            #signature_item
            #ptr_item
            #hook_items
        }
    }
}

/// The closure slot, thunk and `hook()` of a hookable function, inside its
/// module.
fn hook_items(
    parameters: &[Parameter],
    output: &Option<TokenStream>,
    abi: &syn::LitStr,
    shown: &str,
    file: &str,
    to_root: &TokenStream,
    verified: bool,
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
    let verification = if verified {
        quote! { VERIFICATION }
    } else {
        quote! { &[] }
    };
    let mut errors = format!(
        "Fails unless `{file}`'s `init()` has succeeded, if this mod already holds a hook on \
         the function"
    );
    if verified {
        errors.push_str(", if its first bytes differ from `VERIFICATION`");
    }
    errors.push_str(", or if patching fails.");
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
                    &#to_root MODULE,
                    RVA,
                    #verification,
                    &SLOT,
                    ::std::boxed::Box::new(detour),
                    thunk,
                )
            }
        }
    }
}

/// Emits one global as a `static` `imping_runtime::Global`.
fn emit_global(table: &TypeTable<'_>, global: &Global, name: &str) -> TokenStream {
    let module = table.module();
    let depth = global.namespace.len();
    let shown = doc_name(&qualified(&global.namespace, &global.name));
    let doc = doc(format!(
        "`{shown}` at `{}+{:#x}`.",
        doc_name(&module.name),
        global.rva
    ));
    let name = ident(name);
    let ty = table.rust_type(global.type_id, depth);
    let to_root = supers(depth);
    let rva = hex(u64::from(global.rva));
    quote! {
        #doc
        pub static #name: ::imping_runtime::Global<#ty> =
            ::imping_runtime::Global::new(&#to_root MODULE, #rva);
    }
}
