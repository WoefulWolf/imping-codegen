//! Generates Rust bindings from imping IR.
//!
//! [`Generator`] turns an [`imping_ir::Ir`] into formatted Rust source, one
//! `pub mod` per IR module, and returns it as [`Bindings`] with any
//! [`Warning`]s. The generated code depends only on `imping-runtime`.
//!
//! # Examples
//!
//! ```
//! let ir = imping_ir::Ir::from_json_str(r#"{
//!     "format_version": { "major": 1, "minor": 0 },
//!     "modules": [{
//!         "name": "target.exe", "architecture": "x64",
//!         "image_base": "0x140000000", "timestamp": "0x64b8f2a1", "image_size": "0xa2b000",
//!         "types": [
//!             { "id": 0, "name": "float", "kind": "primitive", "primitive": "f32" },
//!             { "id": 1, "name": "Vector2", "kind": "struct", "size": 8, "align": 4,
//!               "fields": [
//!                 { "name": "x", "type": 0, "offset": 0 },
//!                 { "name": "y", "type": 0, "offset": 4 }
//!               ] }
//!         ]
//!     }]
//! }"#)?;
//!
//! let bindings = imping_codegen::Generator::new().generate(&ir)?;
//! assert!(bindings.source().contains("pub mod target"));
//! assert!(bindings.source().contains("pub struct Vector2"));
//! assert!(bindings.warnings().is_empty());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Generated code
//!
//! - Each module gets a `MODULE` identity and an `init()` that checks the
//!   running build against it.
//! - Types, functions and globals go in one module per namespace
//!   (`target::hw::cVec4` for the type `Hw::cVec4`).
//! - Structs become `#[repr(C)]` with explicit padding, bitfield accessors and
//!   compile-time checks on size, alignment and every field offset; unions get
//!   the same checks.
//! - A struct whose IR field is marked as its base class (a struct at offset
//!   0) derefs to that base and implements `imping_runtime::Inherits` for it
//!   and the base's own bases, so `upcast()` turns pointers into base pointers.
//! - Enums become integer newtypes with associated constants, typedefs become
//!   type aliases, and opaque types become byte arrays or zero-sized markers.
//! - Functions become an `unsafe fn` plus a module of the same name with the
//!   function's `RVA`, `Signature`, `ptr()` and `hook()`. A hook's closure gets
//!   the function's `Original` first; `original.forward()` safely makes the
//!   call the target was making. A parameter named like a static or enum type
//!   in its module gets a trailing `_`.
//! - Functions tagged `imping-safe` get a safe `fn` whose pointer parameters
//!   are references, unless a parameter has no safe form (a [`Warning`] says
//!   so). The tag means the function is always fine to call with valid
//!   arguments and keeps none of its pointer arguments after returning.
//! - Globals become `imping_runtime::Global` statics. Structs, unions and enums
//!   whose every bit pattern is valid implement `imping_runtime::Plain`, which
//!   makes their globals safe to read and write.

mod emit;
mod error;
mod format;
mod functions;
mod layout;
mod names;
mod namespaces;
mod types;

pub use crate::error::{Error, Warning};

use imping_ir::Ir;
use std::collections::HashMap;

/// Generates Rust bindings from an [`Ir`].
///
/// Create one with [`Generator::new`], optionally configure it, then call
/// [`generate`](Self::generate).
///
/// # Examples
///
/// ```
/// let generator = imping_codegen::Generator::new()
///     .module_name("TargetCore.dll", "target")
///     .accept_build("TargetCore.dll", 0x64b8f2a1, 0xa3c000);
/// ```
#[derive(Debug, Clone, Default)]
pub struct Generator {
    /// IR module name -> Rust module name overrides, in call order.
    module_names: Vec<(String, String)>,
    /// Extra accepted builds: IR module name, timestamp, image size.
    accepted_builds: Vec<(String, u32, u32)>,
}

impl Generator {
    /// Creates a generator with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Names the Rust module generated for the IR module `ir_name`.
    ///
    /// By default it's the IR module's file name without the extension, in
    /// snake case: `TargetCore.dll` becomes `target_core`. If the same
    /// `ir_name` is given more than once, the last call wins.
    pub fn module_name(mut self, ir_name: impl Into<String>, rust_name: impl Into<String>) -> Self {
        self.module_names.push((ir_name.into(), rust_name.into()));
        self
    }

    /// Makes the generated `init()` of the IR module `ir_name` also accept the
    /// build with this timestamp and image size.
    ///
    /// Meant for the same code in a different image, such as an executable
    /// wrapped by a packer or DRM layer that keeps the timestamp but grows the
    /// image. The bindings promise the IR describes every build they accept, so
    /// only add builds whose code and data match the analysed one.
    pub fn accept_build(
        mut self,
        ir_name: impl Into<String>,
        timestamp: u32,
        image_size: u32,
    ) -> Self {
        self.accepted_builds
            .push((ir_name.into(), timestamp, image_size));
        self
    }

    /// Generates the bindings for every module in `ir`.
    ///
    /// Validates the IR first. Anything the generator can't represent becomes
    /// a [`Warning`] instead of an error.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidIr`] if `ir` fails [`Ir::validate`].
    /// - [`Error::UnknownModule`] if a [`module_name`](Self::module_name) or
    ///   [`accept_build`](Self::accept_build) names a module that isn't in `ir`.
    /// - [`Error::InvalidModuleName`] if a [`module_name`](Self::module_name)
    ///   isn't a valid Rust module name.
    /// - [`Error::ModuleNameClash`] if two IR modules get the same Rust module
    ///   name.
    pub fn generate(&self, ir: &Ir) -> Result<Bindings, Error> {
        ir.validate().map_err(Error::InvalidIr)?;

        let mut overrides: HashMap<&str, &str> = HashMap::new();
        for (ir_name, rust_name) in &self.module_names {
            if !ir.modules.iter().any(|module| module.name == *ir_name) {
                return Err(Error::UnknownModule {
                    option: "module_name",
                    name: ir_name.clone(),
                });
            }
            if !names::is_plain_identifier(rust_name) {
                return Err(Error::InvalidModuleName {
                    name: rust_name.clone(),
                });
            }
            overrides.insert(ir_name, rust_name);
        }

        let mut extra_builds: HashMap<&str, Vec<(u32, u32)>> = HashMap::new();
        for (ir_name, timestamp, image_size) in &self.accepted_builds {
            if !ir.modules.iter().any(|module| module.name == *ir_name) {
                return Err(Error::UnknownModule {
                    option: "accept_build",
                    name: ir_name.clone(),
                });
            }
            extra_builds
                .entry(ir_name)
                .or_default()
                .push((*timestamp, *image_size));
        }

        let mut taken: HashMap<String, &str> = HashMap::new();
        let mut rust_names = Vec::with_capacity(ir.modules.len());
        for module in &ir.modules {
            let rust_name = match overrides.get(module.name.as_str()) {
                Some(name) => (*name).to_owned(),
                None => names::module_name(&module.name),
            };
            if let Some(first) = taken.insert(rust_name.clone(), &module.name) {
                return Err(Error::ModuleNameClash {
                    first: first.to_owned(),
                    second: module.name.clone(),
                    rust_name,
                });
            }
            rust_names.push(rust_name);
        }

        let mut warnings = Vec::new();
        let items = ir
            .modules
            .iter()
            .zip(&rust_names)
            .map(|(module, rust_name)| {
                let extra = extra_builds
                    .get(module.name.as_str())
                    .map_or(&[][..], Vec::as_slice);
                emit::module(module, rust_name, extra, &mut warnings)
            });
        let source = format::render(quote::quote! { #(#items)* });
        Ok(Bindings { source, warnings })
    }
}

/// Generated Rust source plus the warnings raised while generating it.
///
/// Returned by [`Generator::generate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings {
    source: String,
    warnings: Vec<Warning>,
}

impl Bindings {
    /// Returns the generated Rust source, formatted.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Consumes the bindings and returns the generated Rust source.
    pub fn into_source(self) -> String {
        self.source
    }

    /// Returns everything the generator skipped or simplified, in the order
    /// found.
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }
}
