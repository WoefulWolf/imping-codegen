//! A module's type table: lookups, the Rust names of type items, and the Rust
//! type expression for any type id.

use crate::names::{self, Scope};
use imping_ir::{Architecture, CallingConvention, Module, Primitive, TypeDef, TypeId, TypeKind};
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use std::collections::HashMap;

/// How many typedefs [`TypeTable::peel`] follows before assuming a cycle.
const MAX_TYPEDEF_DEPTH: usize = 64;

/// One IR module's types, indexed, with the Rust names of those emitted as
/// items.
pub(crate) struct TypeTable<'a> {
    module: &'a Module,
    by_id: HashMap<u32, &'a TypeDef>,
    /// Rust names of the types emitted as items.
    names: HashMap<u32, String>,
}

impl<'a> TypeTable<'a> {
    /// Indexes `module`'s types and claims a name in `scope` for every type
    /// emitted as an item, lowest type id first; a clashing name gets the
    /// type id as a suffix.
    pub(crate) fn new(module: &'a Module, scope: &mut Scope) -> Self {
        let by_id: HashMap<u32, &TypeDef> =
            module.types.iter().map(|def| (def.id.0, def)).collect();
        let mut table = Self {
            module,
            by_id,
            names: HashMap::new(),
        };

        let mut defs: Vec<&TypeDef> = module.types.iter().collect();
        defs.sort_by_key(|def| def.id.0);
        for def in defs {
            if !table.is_item_kind(def) {
                continue;
            }
            let base = names::sanitize(&def.name);
            let id = def.id.0;
            let name = scope.claim(&base, |n| match n {
                2 => format!("{base}_{id}"),
                _ => format!("{base}_{id}_{n}"),
            });
            table.names.insert(id, name);
        }
        table
    }

    /// The IR module these types belong to.
    pub(crate) fn module(&self) -> &'a Module {
        self.module
    }

    /// The pointer size in bytes of the module's architecture.
    pub(crate) fn pointer_size(&self) -> u64 {
        match self.module.architecture {
            Architecture::X86 => 4,
            Architecture::X64 => 8,
        }
    }

    /// Looks up a type. The IR has been validated, so every referenced id
    /// exists.
    pub(crate) fn get(&self, id: TypeId) -> &'a TypeDef {
        self.by_id
            .get(&id.0)
            .unwrap_or_else(|| panic!("type id {} missing from a validated IR", id.0))
    }

    /// Follows typedefs from `id` to the type they name, or `None` if they
    /// form a cycle.
    pub(crate) fn peel(&self, id: TypeId) -> Option<&'a TypeDef> {
        let mut def = self.get(id);
        for _ in 0..MAX_TYPEDEF_DEPTH {
            match def.kind {
                TypeKind::Typedef { target } => def = self.get(target),
                _ => return Some(def),
            }
        }
        None
    }

    /// Whether a type is emitted as its own item.
    pub(crate) fn is_item(&self, id: TypeId) -> bool {
        self.names.contains_key(&id.0)
    }

    /// The Rust name of a type emitted as an item.
    pub(crate) fn name(&self, id: TypeId) -> &str {
        self.names
            .get(&id.0)
            .unwrap_or_else(|| panic!("type id {} is not an item", id.0))
    }

    /// Whether a type is emitted as its own item, rather than written inline
    /// (primitives, pointers, arrays, function types) or replaced by its
    /// target (typedefs with their target's name, typedefs of function types).
    fn is_item_kind(&self, def: &TypeDef) -> bool {
        match def.kind {
            TypeKind::Struct { .. }
            | TypeKind::Union { .. }
            | TypeKind::Enum { .. }
            | TypeKind::Opaque { .. } => true,
            TypeKind::Primitive { .. }
            | TypeKind::Pointer { .. }
            | TypeKind::Array { .. }
            | TypeKind::Function { .. } => false,
            TypeKind::Typedef { target } => match self.peel(def.id) {
                // A typedef cycle is emitted as an opaque item.
                None => true,
                Some(peeled) if matches!(peeled.kind, TypeKind::Function { .. }) => false,
                Some(_) => names::sanitize(&def.name) != names::sanitize(&self.get(target).name),
            },
        }
    }

    /// The Rust type expression for `id`, written from a module `depth` levels
    /// below the bindings' module root.
    pub(crate) fn rust_type(&self, id: TypeId, depth: usize) -> TokenStream {
        let def = self.get(id);
        match &def.kind {
            TypeKind::Primitive { primitive } => primitive_type(*primitive),
            TypeKind::Pointer { pointee, is_const } => {
                if let Some(peeled) = self.peel(*pointee)
                    && matches!(peeled.kind, TypeKind::Function { .. })
                {
                    return self.function_pointer(peeled, depth);
                }
                if let Some(text) = self.text_kind(*pointee) {
                    return text.pointer();
                }
                let pointee = self.rust_type(*pointee, depth);
                if *is_const {
                    quote! { *const #pointee }
                } else {
                    quote! { *mut #pointee }
                }
            }
            TypeKind::Array { element, count } => {
                let count = proc_macro2::Literal::u64_unsuffixed(*count);
                if let Some(text) = self.text_kind(*element) {
                    return text.buffer(count);
                }
                let element = self.rust_type(*element, depth);
                quote! { [#element; #count] }
            }
            // A function type used as a value decays to a pointer, as in C.
            TypeKind::Function { .. } => self.function_pointer(def, depth),
            TypeKind::Typedef { target } if !self.is_item(id) => self.rust_type(*target, depth),
            TypeKind::Struct { .. }
            | TypeKind::Union { .. }
            | TypeKind::Enum { .. }
            | TypeKind::Opaque { .. }
            | TypeKind::Typedef { .. } => self.path(id, depth),
        }
    }

    /// Whether a type is a text character: through typedefs, a primitive named
    /// `char` (one byte) or `wchar_t` / `wchar16` (two bytes). Pointers to and
    /// arrays of them become the runtime's string types.
    pub(crate) fn text_kind(&self, id: TypeId) -> Option<Text> {
        let def = self.peel(id)?;
        match (def.name.as_str(), &def.kind) {
            (
                "char",
                TypeKind::Primitive {
                    primitive: Primitive::I8 | Primitive::U8,
                },
            ) => Some(Text::Narrow),
            (
                "wchar_t" | "wchar16",
                TypeKind::Primitive {
                    primitive: Primitive::U16 | Primitive::I16,
                },
            ) => Some(Text::Wide),
            _ => None,
        }
    }

    /// The path to a type item from `depth` levels below the module root.
    pub(crate) fn path(&self, id: TypeId, depth: usize) -> TokenStream {
        let name = names::ident(self.name(id));
        let supers = (0..depth).map(|_| quote! { super:: });
        quote! { #(#supers)* #name }
    }

    /// The nullable function-pointer type for a function type.
    fn function_pointer(&self, def: &TypeDef, depth: usize) -> TokenStream {
        let TypeKind::Function {
            calling_convention,
            parameters,
            return_type,
            variadic,
        } = &def.kind
        else {
            unreachable!("function_pointer called on a non-function type");
        };
        let abi = abi(self.module.architecture, *calling_convention, *variadic);
        // A lone `void` parameter means "no parameters", as in C.
        let parameters = parameters
            .iter()
            .filter(|&&parameter| !self.is_void(parameter))
            .map(|&parameter| self.rust_type(parameter, depth));
        let dots = variadic.then(|| quote! { , ... });
        let output = (!self.is_void(*return_type)).then(|| {
            let return_type = self.rust_type(*return_type, depth);
            quote! { -> #return_type }
        });
        quote! {
            ::core::option::Option<unsafe extern #abi fn(#(#parameters),* #dots) #output>
        }
    }

    /// Whether a type is `void`, through typedefs.
    pub(crate) fn is_void(&self, id: TypeId) -> bool {
        matches!(
            self.peel(id).map(|def| &def.kind),
            Some(TypeKind::Primitive {
                primitive: Primitive::Void
            })
        )
    }

    /// The integer primitive an enum or typedef chain resolves to, if any.
    pub(crate) fn integer(&self, id: TypeId) -> Option<Integer> {
        match self.peel(id)?.kind {
            TypeKind::Primitive { primitive } => Integer::from_primitive(primitive),
            _ => None,
        }
    }
}

/// The Rust ABI string for a calling convention.
pub(crate) fn abi(
    architecture: Architecture,
    convention: CallingConvention,
    variadic: bool,
) -> syn::LitStr {
    let abi = match (architecture, convention) {
        (_, _) if variadic => "C",
        (Architecture::X64, _) => "system",
        (Architecture::X86, CallingConvention::Cdecl) => "C",
        (Architecture::X86, CallingConvention::Stdcall) => "stdcall",
        (Architecture::X86, CallingConvention::Fastcall) => "fastcall",
        (Architecture::X86, CallingConvention::Thiscall) => "thiscall",
        // Not a valid x86 convention; the platform default is the best guess.
        (Architecture::X86, CallingConvention::Win64) => "C",
    };
    syn::LitStr::new(abi, Span::call_site())
}

/// The Rust type for a primitive.
fn primitive_type(primitive: Primitive) -> TokenStream {
    match primitive {
        Primitive::Void => quote! { ::core::ffi::c_void },
        Primitive::Bool => quote! { bool },
        Primitive::U8 => quote! { u8 },
        Primitive::U16 => quote! { u16 },
        Primitive::U32 => quote! { u32 },
        Primitive::U64 => quote! { u64 },
        Primitive::I8 => quote! { i8 },
        Primitive::I16 => quote! { i16 },
        Primitive::I32 => quote! { i32 },
        Primitive::I64 => quote! { i64 },
        Primitive::F32 => quote! { f32 },
        Primitive::F64 => quote! { f64 },
    }
}

/// The character width of a text type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Text {
    /// `char`: narrow, NUL-terminated text.
    Narrow,
    /// `wchar_t`: UTF-16, NUL-terminated text.
    Wide,
}

impl Text {
    /// The runtime type for a pointer to this text.
    pub(crate) fn pointer(self) -> TokenStream {
        match self {
            Text::Narrow => quote! { ::imping_runtime::CStrPtr },
            Text::Wide => quote! { ::imping_runtime::WStrPtr },
        }
    }

    /// The runtime type for a fixed-size buffer of `count` characters.
    pub(crate) fn buffer(self, count: proc_macro2::Literal) -> TokenStream {
        match self {
            Text::Narrow => quote! { ::imping_runtime::CStrBuf<#count> },
            Text::Wide => quote! { ::imping_runtime::WStrBuf<#count> },
        }
    }
}

/// An integer primitive's width and signedness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Integer {
    /// The width in bits: 8, 16, 32 or 64.
    pub(crate) bits: u32,
    /// Whether it is signed.
    pub(crate) signed: bool,
}

impl Integer {
    /// The integer a primitive is, if it is one.
    pub(crate) fn from_primitive(primitive: Primitive) -> Option<Self> {
        let (bits, signed) = match primitive {
            Primitive::U8 => (8, false),
            Primitive::U16 => (16, false),
            Primitive::U32 => (32, false),
            Primitive::U64 => (64, false),
            Primitive::I8 => (8, true),
            Primitive::I16 => (16, true),
            Primitive::I32 => (32, true),
            Primitive::I64 => (64, true),
            _ => return None,
        };
        Some(Self { bits, signed })
    }

    /// The unsigned integer of the same width.
    pub(crate) fn unsigned(bits: u32) -> Self {
        Self {
            bits,
            signed: false,
        }
    }

    /// The Rust type name, e.g. `u32`.
    pub(crate) fn ident(self) -> proc_macro2::Ident {
        let prefix = if self.signed { "i" } else { "u" };
        format_ident!("{prefix}{}", self.bits)
    }

    /// The width in bytes.
    pub(crate) fn bytes(self) -> u64 {
        u64::from(self.bits / 8)
    }
}
