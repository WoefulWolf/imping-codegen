//! Emits the Rust items for one IR module.
//!
//! Expressions only get parentheses where precedence needs them, so the output
//! triggers no `unused_parens` warnings in a user's build.

use crate::Warning;
use crate::functions;
use crate::layout::{BitKind, BitMember, Layouts, Member, Plan, Repr, StructPlan, UnionPlan};
use crate::names::{self, Scope, doc_name, ident};
use crate::namespaces::Namespace;
use crate::types::{Integer, TypeTable};
use imping_ir::{Architecture, EnumVariant, Field, Module, TypeDef, TypeId, TypeKind};
use proc_macro2::{Ident, Literal, Span, TokenStream};
use quote::quote;
use std::collections::HashSet;

/// Emits the `pub mod` for one IR module: its identity and `init`, then its
/// types, functions and globals, each in its namespace's module.
///
/// `extra_builds` are the builds (timestamp, image size) accepted besides the
/// IR's own.
pub(crate) fn module(
    module: &Module,
    rust_name: &str,
    extra_builds: &[(u32, u32)],
    warnings: &mut Vec<Warning>,
) -> TokenStream {
    let mut scope = Scope::default();
    scope.reserve("MODULE");
    scope.reserve("init");
    scope.reserve("init_any_build");
    // Function and global namespaces go first, so type namespaces can't rename
    // the modules they use.
    let mut root = Namespace::root(scope);
    for function in &module.functions {
        root.register(&function.namespace);
    }
    for global in &module.globals {
        root.register(&global.namespace);
    }
    let mut table = TypeTable::new(module, &mut root);
    root.name_modules();
    table.resolve_modules(&root);
    let layouts = Layouts::plan(&table);

    for def in &module.types {
        if let Some(item) = type_item(&table, &layouts, def, warnings) {
            root.at(&def.namespace).items.push(item);
        }
    }
    let code = functions::emit(&table, &layouts, root, warnings);
    for vtable in &module.vtables {
        warnings.push(Warning::VtableSkipped {
            module: module.name.clone(),
            name: qualified(&vtable.namespace, &vtable.name),
        });
    }

    let architecture = match module.architecture {
        Architecture::X86 => "x86",
        Architecture::X64 => "x64",
    };
    let doc = doc(format!(
        "Bindings for `{}` ({architecture}, timestamp `{:#x}`, image size `{:#x}`).",
        doc_name(&module.name),
        module.timestamp,
        module.image_size
    ));
    let identity = identity(module, extra_builds, architecture);
    let name = ident(rust_name);
    quote! {
        #doc
        #[allow(non_snake_case, non_camel_case_types, non_upper_case_globals, dead_code, clippy::all)]
        pub mod #name {
            #identity
            #code
        }
    }
}

/// Returns `MODULE`, `init()` and the compile-time target architecture check.
fn identity(module: &Module, extra_builds: &[(u32, u32)], architecture: &str) -> TokenStream {
    let file = doc_name(&module.name);
    let name = Literal::string(&module.name);
    // The IR's own build first, then the extra ones, without repeats.
    let mut builds = vec![(module.timestamp, module.image_size)];
    for build in extra_builds {
        if !builds.contains(build) {
            builds.push(*build);
        }
    }
    let builds = builds.iter().map(|&(timestamp, image_size)| {
        let timestamp = hex(u64::from(timestamp));
        let image_size = hex(u64::from(image_size));
        quote! { ::imping_runtime::Build { timestamp: #timestamp, image_size: #image_size } }
    });
    let module_doc = doc(format!(
        "The builds of `{file}` these bindings accept, the one they were generated from first."
    ));
    let init_doc = docs(&[
        format!(
            "Finds `{file}` in this process and checks it is a build these bindings accept. \
             Call it before calling or hooking anything in this module; once it has succeeded, \
             calling it again does nothing."
        ),
        "# Errors".to_owned(),
        format!(
            "Fails if `{file}` is not loaded or is none of the accepted builds; see \
             `imping_runtime::Module::init`. It can be called again later, for instance once \
             a DLL has been loaded."
        ),
        "# Safety".to_owned(),
        format!(
            "The IR these bindings were generated from must correctly describe every build of \
             `{file}` they accept: addresses, function signatures and type layouts. `init` \
             checks the build's identity (timestamp and image size); the rest is the caller's \
             promise, which is what makes every `hook` in this module safe."
        ),
    ]);
    let any_build_doc = docs(&[
        format!(
            "Finds `{file}` in this process and initializes it whatever build it is, returning              the build found; for trying a build these bindings don't accept. `init` is the              checked way in."
        ),
        "# Errors".to_owned(),
        format!("Fails if `{file}` is not loaded; see `imping_runtime::Module::init_any_build`."),
        "# Safety".to_owned(),
        format!(
            "Everything done through this module must be correct for the build of `{file}`              that is loaded, which nothing has checked. Each `hook` still compares the              function's first bytes where the IR has them; calls and globals are used as they are."
        ),
    ]);
    let (target_arch, label) = match module.architecture {
        Architecture::X86 => ("x86", "an x86"),
        Architecture::X64 => ("x86_64", "an x64"),
    };
    let message = Literal::string(&format!(
        "these bindings are for {architecture} `{}`; build the mod for {label} target",
        module.name
    ));
    quote! {
        #module_doc
        pub static MODULE: ::imping_runtime::Module =
            ::imping_runtime::Module::new(#name, &[#(#builds),*]);

        #init_doc
        pub unsafe fn init() -> ::core::result::Result<(), ::imping_runtime::Error> {
            #[comment = " SAFETY: forwarded to the caller."]
            unsafe { MODULE.init() }
        }

        #any_build_doc
        pub unsafe fn init_any_build() -> ::core::result::Result<::imping_runtime::Build, ::imping_runtime::Error> {
            #[comment = " SAFETY: forwarded to the caller."]
            unsafe { MODULE.init_any_build() }
        }

        const _: () = ::core::assert!(::core::cfg!(target_arch = #target_arch), #message);
    }
}

/// Emits the item for a type, if it has one.
fn type_item(
    table: &TypeTable<'_>,
    layouts: &Layouts,
    def: &TypeDef,
    warnings: &mut Vec<Warning>,
) -> Option<TokenStream> {
    let item = match &def.kind {
        TypeKind::Primitive { .. }
        | TypeKind::Pointer { .. }
        | TypeKind::Array { .. }
        | TypeKind::Function { .. } => return None,
        // Not an item: written as its target wherever it is used.
        TypeKind::Typedef { .. } if !table.is_item(def.id) => return None,
        TypeKind::Typedef { target } => match table.peel(def.id) {
            Some(_) => typedef(table, def, *target),
            None => {
                warn_opaque(table, def, "it is part of a typedef cycle", warnings);
                opaque_marker(table, def, "part of a typedef cycle in the IR; opaque.")
            }
        },
        TypeKind::Struct { size, fields, .. } | TypeKind::Union { size, fields, .. } => {
            match layouts.get(def.id) {
                Plan::Struct(plan) => structure(table, layouts, def, *size, fields, plan),
                Plan::Union(plan) => union(table, def, *size, fields, plan),
                Plan::Opaque {
                    size,
                    align,
                    reason,
                } => {
                    warn_opaque(table, def, reason, warnings);
                    let explanation = format!(
                        "{size} bytes, align {align}. Its layout can't be represented \
                         ({reason}), so it is opaque bytes."
                    );
                    opaque_bytes(table, def, *size, *align, &explanation)
                }
            }
        }
        TypeKind::Enum {
            underlying,
            variants,
        } => enumeration(table, def, *underlying, variants, warnings),
        TypeKind::Opaque { size: Some(size) } => {
            opaque_bytes(table, def, *size, 1, &format!("opaque, {size} bytes."))
        }
        TypeKind::Opaque { size: None } => opaque_marker(
            table,
            def,
            "opaque, size unknown; use only behind pointers.",
        ),
    };
    let plain = plain_impl(table, layouts, def);
    Some(quote! { #item #plain })
}

/// Returns `unsafe impl Plain` for a struct, union, enum or opaque type valid
/// for every bit pattern, which makes `Global::read` and `write` safe for it.
fn plain_impl(table: &TypeTable<'_>, layouts: &Layouts, def: &TypeDef) -> Option<TokenStream> {
    let reason = match &def.kind {
        TypeKind::Enum { .. } => " SAFETY: a transparent wrapper of an integer.",
        TypeKind::Opaque { size: Some(_) } => " SAFETY: only bytes.",
        TypeKind::Struct { .. } | TypeKind::Union { .. } => match layouts.get(def.id) {
            Plan::Opaque { .. } => " SAFETY: only bytes.",
            _ => " SAFETY: every field is plain data, so every bit pattern is a valid value.",
        },
        _ => return None,
    };
    if !layouts.is_plain(table, def.id) {
        return None;
    }
    let name = ident(table.name(def.id));
    Some(quote! {
        #[comment = #reason]
        unsafe impl ::imping_runtime::Plain for #name {}
    })
}

fn warn_opaque(table: &TypeTable<'_>, def: &TypeDef, reason: &str, warnings: &mut Vec<Warning>) {
    warnings.push(Warning::TypeMadeOpaque {
        module: table.module().name.clone(),
        type_name: table.source_name(def.id),
        reason: reason.to_owned(),
    });
}

/// Returns a type's source name with its namespace, for its docs.
fn shown(table: &TypeTable<'_>, def: &TypeDef) -> String {
    doc_name(&table.source_name(def.id))
}

fn typedef(table: &TypeTable<'_>, def: &TypeDef, target: TypeId) -> TokenStream {
    let name = ident(table.name(def.id));
    let target = table.rust_type(target, def.namespace.len());
    let doc = doc(format!("Typedef `{}`.", shown(table, def)));
    quote! {
        #doc
        pub type #name = #target;
    }
}

/// Returns the struct's base-class field: the one the IR marks as the base, if
/// it's a struct at offset 0 and the struct isn't packed.
///
/// A packed struct's base may sit at an address its type can't be aligned to.
/// Any other `base` mark is ignored.
fn base_field<'a>(
    table: &TypeTable<'a>,
    layouts: &Layouts,
    id: TypeId,
) -> Option<(usize, &'a Field)> {
    let TypeKind::Struct { fields, .. } = &table.get(id).kind else {
        return None;
    };
    let Plan::Struct(plan) = layouts.get(id) else {
        return None;
    };
    if plan.repr == Repr::Packed {
        return None;
    }
    let (index, field) = fields.iter().enumerate().find(|(_, field)| field.base)?;
    let is_struct = table
        .peel(field.type_id)
        .is_some_and(|def| matches!(def.kind, TypeKind::Struct { .. }));
    let emitted = plan
        .members
        .iter()
        .any(|member| matches!(member, Member::Field { index: i, .. } if *i == index));
    (field.offset == 0 && field.bits.is_none() && is_struct && emitted).then_some((index, field))
}

/// Returns the base classes of a struct whose base field is `base`: the direct
/// base first, then each base's own base, as written (typedefs kept).
fn ancestors(table: &TypeTable<'_>, layouts: &Layouts, base: &Field) -> Vec<TypeId> {
    let mut chain = vec![base.type_id];
    let mut seen = HashSet::new();
    let mut current = table.peel(base.type_id).map(|def| def.id);
    while let Some(id) = current {
        if !seen.insert(id.0) {
            break;
        }
        let Some((_, next)) = base_field(table, layouts, id) else {
            break;
        };
        chain.push(next.type_id);
        current = table.peel(next.type_id).map(|def| def.id);
    }
    chain
}

fn structure(
    table: &TypeTable<'_>,
    layouts: &Layouts,
    def: &TypeDef,
    size: u64,
    fields: &[Field],
    plan: &StructPlan,
) -> TokenStream {
    let name = ident(table.name(def.id));
    let depth = def.namespace.len();
    let base = base_field(table, layouts, def.id);
    let mut scope = Scope::default();
    let mut methods = Scope::default();
    let mut members = Vec::new();
    let mut offsets = Vec::new();
    let mut accessors = Vec::new();
    let mut base_ident = None;

    for member in &plan.members {
        match member {
            Member::Field { index, offset } => {
                let field = &fields[*index];
                let field_ident = ident(&scope.claim_numbered(&names::sanitize(&field.name)));
                let ty = table.rust_type(field.type_id, depth);
                let doc = doc(format!("`{}` at `{offset:#x}`.", doc_name(&field.name)));
                members.push(quote! { #doc pub #field_ident: #ty });
                if base.is_some_and(|(base_index, _)| base_index == *index) {
                    base_ident = Some(field_ident.clone());
                }
                offsets.push((field_ident, *offset));
            }
            Member::Padding { offset, len } => {
                let field_ident = ident(&scope.claim_numbered(&format!("_pad_{offset:#x}")));
                let doc = doc(format!("Unknown bytes `{offset:#x}..{:#x}`.", offset + len));
                let len = Literal::u64_unsuffixed(*len);
                members.push(quote! { #doc pub #field_ident: [u8; #len] });
            }
            Member::Bits {
                offset,
                storage,
                fields: bits,
            } => {
                let field_ident = ident(&scope.claim_numbered(&format!("_bitfield_{offset:#x}")));
                let listed: Vec<String> = bits
                    .iter()
                    .map(|bit| format!("`{}`", doc_name(&fields[bit.index].name)))
                    .collect();
                let doc = doc(format!(
                    "Storage at `{offset:#x}` for the bitfields {}; use their accessor methods.",
                    join_words(&listed)
                ));
                let storage_ty = storage.ident();
                members.push(quote! { #doc pub #field_ident: #storage_ty });
                for bit in bits {
                    let accessor = BitAccessor {
                        table,
                        field: &fields[bit.index],
                        bit,
                        storage_field: &field_ident,
                        storage: *storage,
                        offset: *offset,
                        depth,
                    };
                    accessors.push(accessor.emit(&mut methods));
                }
                offsets.push((field_ident, *offset));
            }
        }
    }

    let (repr, note) = repr_attribute(plan.repr);
    let mut paragraphs = vec![format!(
        "`{}`: {size} bytes, align {}.",
        shown(table, def),
        plan.align
    )];
    let inheritance = base.zip(base_ident).map(|((_, field), field_ident)| {
        let chain = ancestors(table, layouts, field);
        let base_name = table.peel(field.type_id).map_or_else(
            || table.source_name(field.type_id),
            |def| table.source_name(def.id),
        );
        paragraphs.push(format!(
            "Derives from `{}`, its field `{}`: it derefs to it, and implements \
             `imping_runtime::Inherits` for it and its own bases.",
            doc_name(&base_name),
            doc_name(&field.name)
        ));
        inheritance(table, &name, &field_ident, &chain, depth)
    });
    paragraphs.extend(note);
    let doc = docs(&paragraphs);
    let default = zeroed_default(&name);
    let accessors = (!accessors.is_empty()).then(|| {
        quote! {
            impl #name {
                #(#accessors)*
            }
        }
    });
    let asserts = layout_asserts(&name, size, plan.align, &offsets);
    quote! {
        #doc
        #repr
        #[derive(Clone, Copy, Debug)]
        pub struct #name {
            #(#members),*
        }
        #default
        #accessors
        #inheritance
        #asserts
    }
}

/// Returns `Deref` and `DerefMut` to a struct's base field, and `Inherits` for
/// each type in `chain`: the direct base first, then its bases in turn.
fn inheritance(
    table: &TypeTable<'_>,
    name: &Ident,
    field: &Ident,
    chain: &[TypeId],
    depth: usize,
) -> TokenStream {
    let base = table.rust_type(chain[0], depth);
    let names: Vec<String> = chain
        .iter()
        .map(|&id| {
            let shown = table.peel(id).map_or(id, |def| def.id);
            format!("`{}`", doc_name(&table.source_name(shown)))
        })
        .collect();
    let impls = chain.iter().enumerate().map(|(level, &id)| {
        let ty = table.rust_type(id, depth);
        let reason = if level == 0 {
            format!(" SAFETY: `{name}` is `repr(C)` and its field `{field}` at offset 0 is its base {}.", names[0])
        } else {
            format!(
                " SAFETY: `{name}` is `repr(C)` and begins with {}, each at offset 0.",
                names[..=level].join(", which begins with ")
            )
        };
        quote! {
            #[comment = #reason]
            unsafe impl ::imping_runtime::Inherits<#ty> for #name {}
        }
    });
    quote! {
        impl ::core::ops::Deref for #name {
            type Target = #base;
            fn deref(&self) -> &Self::Target {
                &self.#field
            }
        }
        impl ::core::ops::DerefMut for #name {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.#field
            }
        }
        #(#impls)*
    }
}

fn union(
    table: &TypeTable<'_>,
    def: &TypeDef,
    size: u64,
    fields: &[Field],
    plan: &UnionPlan,
) -> TokenStream {
    let name = ident(table.name(def.id));
    let mut scope = Scope::default();
    let mut members: Vec<TokenStream> = plan
        .fields
        .iter()
        .map(|&index| {
            let field = &fields[index];
            let field_ident = ident(&scope.claim_numbered(&names::sanitize(&field.name)));
            let ty = table.rust_type(field.type_id, def.namespace.len());
            let doc = doc(format!("`{}` at `0x0`.", doc_name(&field.name)));
            quote! { #doc pub #field_ident: #ty }
        })
        .collect();
    if let Some(len) = plan.padding {
        let field_ident = ident(&scope.claim_numbered("_pad_0x0"));
        let doc = doc(format!(
            "Unknown bytes `0x0..{len:#x}`, giving the union its full size."
        ));
        let len = Literal::u64_unsuffixed(len);
        members.push(quote! { #doc pub #field_ident: [u8; #len] });
    }

    let (repr, note) = repr_attribute(plan.repr);
    let mut paragraphs = vec![format!(
        "`{}`: union, {size} bytes, align {}.",
        shown(table, def),
        plan.align
    )];
    paragraphs.extend(note);
    let doc = docs(&paragraphs);
    let default = zeroed_default(&name);
    let debug_name = Literal::string(table.name(def.id));
    let asserts = layout_asserts(&name, size, plan.align, &[]);
    quote! {
        #doc
        #repr
        #[derive(Clone, Copy)]
        pub union #name {
            #(#members),*
        }
        #default
        impl ::core::fmt::Debug for #name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.debug_struct(#debug_name).finish_non_exhaustive()
            }
        }
        #asserts
    }
}

fn enumeration(
    table: &TypeTable<'_>,
    def: &TypeDef,
    underlying: TypeId,
    variants: &[EnumVariant],
    warnings: &mut Vec<Warning>,
) -> TokenStream {
    let integer = table
        .integer(underlying)
        .expect("a validated enum has an integer underlying type");
    let bare_name = table.name(def.id);
    let name = ident(bare_name);
    let ty = integer.ident();

    let mut scope = Scope::default();
    let mut seen_values = HashSet::new();
    let mut constants = Vec::new();
    let mut arms = Vec::new();
    for variant in variants {
        let const_name = scope.claim_numbered(&names::sanitize(&variant.name));
        let const_ident = ident(&const_name);
        let (value, truncated) = enum_value(variant.value, integer);
        let mut text = format!("`{}` = {}", doc_name(&variant.name), variant.value);
        if truncated {
            text.push_str(&format!(" in the IR, truncated to fit `{ty}`"));
            warnings.push(Warning::EnumValueTruncated {
                module: table.module().name.clone(),
                enum_name: table.source_name(def.id),
                variant: variant.name.clone(),
                value: variant.value,
            });
        }
        text.push('.');
        let doc = doc(text);
        let literal = value_tokens(value);
        constants.push(quote! { #doc pub const #const_ident: Self = Self(#literal); });
        if seen_values.insert(value) {
            let label = Literal::string(&const_name);
            arms.push(quote! { Self::#const_ident => f.write_str(#label), });
        }
    }

    let doc = doc(format!("`{}`: enum over `{ty}`.", shown(table, def)));
    let fallback = Literal::string(&format!("{bare_name}({{value}})"));
    quote! {
        #doc
        #[repr(transparent)]
        #[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct #name(pub #ty);
        impl #name {
            #(#constants)*
        }
        impl ::core::fmt::Debug for #name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                match *self {
                    #(#arms)*
                    Self(value) => ::core::write!(f, #fallback),
                }
            }
        }
    }
}

/// Fits an IR enum value into the underlying integer.
///
/// A value that fits the other signedness of the same width wraps (two's
/// complement, as in C); anything else is truncated, and the `bool` says so.
fn enum_value(value: i64, integer: Integer) -> (i128, bool) {
    let bits = integer.bits;
    let value = i128::from(value);
    let half = 1i128 << (bits - 1);
    let full = 1i128 << bits;
    let (min, max) = if integer.signed {
        (-half, half - 1)
    } else {
        (0, full - 1)
    };
    if (min..=max).contains(&value) {
        return (value, false);
    }
    let fits_other_signedness = if integer.signed {
        (0..full).contains(&value)
    } else {
        (-half..0).contains(&value)
    };
    let unsigned = value & (full - 1);
    let wrapped = if integer.signed && unsigned > max {
        unsigned - full
    } else {
        unsigned
    };
    (wrapped, !fits_other_signedness)
}

/// Returns a possibly negative integer literal.
fn value_tokens(value: i128) -> TokenStream {
    let literal = Literal::u128_unsuffixed(value.unsigned_abs());
    if value < 0 {
        quote! { -#literal }
    } else {
        quote! { #literal }
    }
}

/// Returns an opaque struct of `size` bytes, for a struct or union whose
/// layout can't be represented or an IR opaque type of known size.
fn opaque_bytes(
    table: &TypeTable<'_>,
    def: &TypeDef,
    size: u64,
    align: u64,
    explanation: &str,
) -> TokenStream {
    let name = ident(table.name(def.id));
    let field_doc = doc(format!("Unknown bytes `0x0..{size:#x}`."));
    let item_doc = doc(format!("`{}`: {explanation}", shown(table, def)));
    let repr = if align > 1 {
        repr_attribute(Repr::Align(align)).0
    } else {
        repr_attribute(Repr::C).0
    };
    let len = Literal::u64_unsuffixed(size);
    let default = zeroed_default(&name);
    let asserts = layout_asserts(&name, size, align, &[]);
    quote! {
        #item_doc
        #repr
        #[derive(Clone, Copy, Debug)]
        pub struct #name {
            #field_doc
            pub _opaque: [u8; #len],
        }
        #default
        #asserts
    }
}

/// Returns a zero-sized, `!Send`, `!Sync`, `!Unpin` type for use behind
/// pointers only.
fn opaque_marker(table: &TypeTable<'_>, def: &TypeDef, explanation: &str) -> TokenStream {
    let name = ident(table.name(def.id));
    let doc = doc(format!("`{}`: {explanation}", shown(table, def)));
    quote! {
        #doc
        #[repr(C)]
        pub struct #name {
            _private: [u8; 0],
            _marker: ::core::marker::PhantomData<(*mut u8, ::core::marker::PhantomPinned)>,
        }
    }
}

/// The getter and setter of one bitfield.
struct BitAccessor<'t, 'a> {
    table: &'t TypeTable<'a>,
    field: &'t Field,
    bit: &'t BitMember,
    storage_field: &'t Ident,
    storage: Integer,
    offset: u64,
    /// How many modules below the bindings' root the struct is emitted.
    depth: usize,
}

/// An expression, and whether it needs parentheses as an operand.
struct Expr {
    tokens: TokenStream,
    compound: bool,
}

impl Expr {
    fn simple(tokens: TokenStream) -> Self {
        Self {
            tokens,
            compound: false,
        }
    }

    fn compound(tokens: TokenStream) -> Self {
        Self {
            tokens,
            compound: true,
        }
    }

    /// Returns the expression as an operand, parenthesised if compound.
    fn operand(&self) -> TokenStream {
        let tokens = &self.tokens;
        if self.compound {
            quote! { (#tokens) }
        } else {
            quote! { #tokens }
        }
    }
}

impl BitAccessor<'_, '_> {
    fn emit(&self, methods: &mut Scope) -> TokenStream {
        let getter_name = methods.claim_numbered(&names::sanitize(&self.field.name));
        let setter_name = methods.claim_numbered(&format!("set_{getter_name}"));
        let getter = ident(&getter_name);
        let setter = ident(&setter_name);

        let (ty, read, bits) = match self.bit.kind {
            BitKind::Bool => {
                let raw = self.raw().operand();
                (
                    quote! { bool },
                    quote! { #raw != 0 },
                    self.to_storage(quote! { value }, None),
                )
            }
            BitKind::Integer(integer) => (
                self.table.rust_type(self.field.type_id, self.depth),
                self.read_integer(integer).tokens,
                self.to_storage(quote! { value }, Some(integer)),
            ),
            BitKind::Enum { id, integer } => {
                let path = self.table.path(id, self.depth);
                let inner = self.read_integer(integer).tokens;
                (
                    path.clone(),
                    quote! { #path(#inner) },
                    self.to_storage(quote! { value.0 }, Some(integer)),
                )
            }
        };
        let write = self.write(&bits);

        let (off, width) = (self.bit.bit_offset, self.bit.width);
        let range = if width == 1 {
            format!("bit {off}")
        } else {
            format!("bits {off}..{}", off + width)
        };
        let getter_doc = doc(format!(
            "Bitfield `{}`: {range} of the storage at `{:#x}`.",
            doc_name(&self.field.name),
            self.offset
        ));
        let setter_doc = doc(format!(
            "Sets bitfield `{}`; bits beyond its {width}-bit width are discarded.",
            doc_name(&self.field.name)
        ));
        quote! {
            #getter_doc
            pub fn #getter(&self) -> #ty {
                #read
            }
            #setter_doc
            pub fn #setter(&mut self, value: #ty) {
                #write
            }
        }
    }

    /// Returns `self.<storage>`.
    fn storage_value(&self) -> TokenStream {
        let field = self.storage_field;
        quote! { self.#field }
    }

    /// Returns the bitfield's mask, unshifted.
    fn mask(&self) -> u64 {
        if self.bit.width == 64 {
            u64::MAX
        } else {
            (1u64 << self.bit.width) - 1
        }
    }

    /// Returns the bitfield's bits, shifted down, in the unsigned storage type.
    fn raw(&self) -> Expr {
        let s = self.storage_value();
        let mask = hex(self.mask());
        if self.bit.width == self.storage.bits {
            Expr::simple(s)
        } else if self.bit.bit_offset == 0 {
            Expr::compound(quote! { #s & #mask })
        } else {
            let off = Literal::u32_unsuffixed(self.bit.bit_offset);
            Expr::compound(quote! { (#s >> #off) & #mask })
        }
    }

    /// Returns the bitfield read as `integer`, sign-extending a signed one.
    fn read_integer(&self, integer: Integer) -> Expr {
        let n = self.storage.bits;
        let value = if integer.signed {
            let s = self.storage_value();
            let signed_storage = Integer {
                bits: n,
                signed: true,
            }
            .ident();
            let left = n - self.bit.bit_offset - self.bit.width;
            let right = n - self.bit.width;
            let base = if left == 0 {
                Expr::compound(quote! { #s as #signed_storage })
            } else {
                let left = Literal::u32_unsuffixed(left);
                Expr::compound(quote! { (#s << #left) as #signed_storage })
            };
            if right == 0 {
                base
            } else {
                let base = base.operand();
                let right = Literal::u32_unsuffixed(right);
                Expr::compound(quote! { #base >> #right })
            }
        } else {
            self.raw()
        };
        if integer.bits == n {
            value
        } else {
            let value = value.operand();
            let ty = integer.ident();
            Expr::compound(quote! { #value as #ty })
        }
    }

    /// Returns `value` converted to the unsigned storage type.
    fn to_storage(&self, value: TokenStream, integer: Option<Integer>) -> Expr {
        match integer {
            Some(integer) if !integer.signed && integer.bits == self.storage.bits => {
                Expr::simple(value)
            }
            _ => {
                let storage_ty = self.storage.ident();
                Expr::compound(quote! { #value as #storage_ty })
            }
        }
    }

    /// Returns the statement storing `bits` into the bitfield.
    fn write(&self, bits: &Expr) -> TokenStream {
        let s = self.storage_value();
        if self.bit.width == self.storage.bits {
            let bits = &bits.tokens;
            return quote! { #s = #bits; };
        }
        let mask = hex(self.mask());
        let clear = hex(self.mask() << self.bit.bit_offset);
        let bits = bits.operand();
        let shifted = if self.bit.bit_offset == 0 {
            quote! { #bits & #mask }
        } else {
            let off = Literal::u32_unsuffixed(self.bit.bit_offset);
            quote! { (#bits & #mask) << #off }
        };
        quote! { #s = (#s & !#clear) | (#shifted); }
    }
}

/// Returns an all-zero `impl Default`, valid for every generated type.
fn zeroed_default(name: &Ident) -> TokenStream {
    quote! {
        impl ::core::default::Default for #name {
            #[doc = " All fields zero."]
            fn default() -> Self {
                #[comment = " SAFETY: every field type is valid when all-zero."]
                unsafe { ::core::mem::zeroed() }
            }
        }
    }
}

/// Returns compile-time assertions on a type's size, alignment and member
/// offsets.
fn layout_asserts(name: &Ident, size: u64, align: u64, offsets: &[(Ident, u64)]) -> TokenStream {
    let size = hex(size);
    let align = hex(align);
    let offsets = offsets.iter().map(|(field, offset)| {
        let offset = hex(*offset);
        quote! { ::core::assert!(::core::mem::offset_of!(#name, #field) == #offset); }
    });
    quote! {
        const _: () = {
            ::core::assert!(::core::mem::size_of::<#name>() == #size);
            ::core::assert!(::core::mem::align_of::<#name>() == #align);
            #(#offsets)*
        };
    }
}

/// Returns the `repr` attribute for a layout, plus a doc paragraph explaining
/// it.
fn repr_attribute(repr: Repr) -> (TokenStream, Option<String>) {
    match repr {
        Repr::C => (quote! { #[repr(C)] }, None),
        Repr::Packed => (
            quote! { #[repr(C, packed)] },
            Some("Packed: the IR places a member where `repr(C)` alignment can't.".to_owned()),
        ),
        Repr::Align(align) => {
            let align = Literal::u64_unsuffixed(align);
            (quote! { #[repr(C, align(#align))] }, None)
        }
    }
}

/// Returns a one-line doc attribute.
pub(crate) fn doc(text: String) -> TokenStream {
    let text = format!(" {text}");
    quote! { #[doc = #text] }
}

/// Returns a doc comment of several paragraphs.
pub(crate) fn docs(paragraphs: &[String]) -> TokenStream {
    let lines = paragraphs.iter().enumerate().map(|(i, paragraph)| {
        let line = doc(paragraph.clone());
        if i == 0 {
            line
        } else {
            quote! { #[doc = ""] #line }
        }
    });
    quote! { #(#lines)* }
}

/// Returns a hexadecimal integer literal.
pub(crate) fn hex(value: u64) -> syn::LitInt {
    syn::LitInt::new(&format!("{value:#x}"), Span::call_site())
}

/// Joins words as `a`, `a and b` or `a, b and c`.
fn join_words(words: &[String]) -> String {
    match words {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Returns a namespaced name for messages, such as `ns::Entity`.
pub(crate) fn qualified(namespace: &[String], name: &str) -> String {
    namespace
        .iter()
        .map(String::as_str)
        .chain([name])
        .collect::<Vec<_>>()
        .join("::")
}
