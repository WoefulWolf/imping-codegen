//! Rust code generation from IR for the imping modding suite.
//!
//! Turns an [`imping_ir::Ir`] into Rust source, one `pub mod` per IR module.
//! The generated code depends only on the `imping-runtime` crate.
//!
//! - Each module gets a `MODULE` identity and an `init()` that checks the
//!   running build against it.
//! - Structs become `#[repr(C)]` structs with explicit padding, bitfield
//!   accessors and compile-time assertions on size, alignment and every field
//!   offset; unions get the same assertions.
//! - Enums become integer newtypes with associated constants.
//! - Typedefs become type aliases, and opaque types become byte arrays or
//!   zero-sized markers.
//! - Functions become an `unsafe fn` plus a module of the same name with the
//!   function's `RVA`, `Signature`, `ptr()` and `hook()`, inside one module per
//!   namespace.
//! - Globals become `imping_runtime::Global` statics.
//!
//! # Example
//!
//! ```
//! let ir = imping_ir::Ir::from_json_str(r#"{
//!     "format_version": { "major": 1, "minor": 0 },
//!     "modules": [{
//!         "name": "game.exe", "architecture": "x64",
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
//! assert!(bindings.source().contains("pub mod game"));
//! assert!(bindings.source().contains("pub struct Vector2"));
//! assert!(bindings.warnings().is_empty());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod emit;
mod error;
mod format;
mod functions;
mod layout;
mod names;
mod types;

pub use crate::error::{Error, Warning};

use imping_ir::Ir;
use std::collections::HashMap;

/// Generates Rust bindings from an [`Ir`].
///
/// Create one with [`Generator::new`], optionally configure it, then call
/// [`generate`](Self::generate).
#[derive(Debug, Clone, Default)]
pub struct Generator {
    /// IR module name -> Rust module name overrides, in call order.
    module_names: Vec<(String, String)>,
}

impl Generator {
    /// Creates a generator with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Names the Rust module generated for the IR module `ir_name`.
    ///
    /// By default the name is derived from the IR module's file name: the
    /// extension is dropped and the rest converted to snake case, so
    /// `GameAssembly.dll` becomes `game_assembly`. If the same `ir_name` is
    /// given more than once, the last call wins.
    ///
    /// ```
    /// let generator = imping_codegen::Generator::new().module_name("GameAssembly.dll", "game");
    /// ```
    pub fn module_name(mut self, ir_name: impl Into<String>, rust_name: impl Into<String>) -> Self {
        self.module_names.push((ir_name.into(), rust_name.into()));
        self
    }

    /// Generates the bindings for every module in `ir`.
    ///
    /// The IR is validated first. Anything the generator can't represent is
    /// reported as a [`Warning`] rather than failing generation.
    ///
    /// # Errors
    ///
    /// - [`Error::InvalidIr`] if `ir` fails [`Ir::validate`].
    /// - [`Error::UnknownModule`] if a [`module_name`](Self::module_name)
    ///   names a module that isn't in `ir`.
    /// - [`Error::InvalidModuleName`] if a [`module_name`](Self::module_name)
    ///   isn't a valid Rust module name.
    /// - [`Error::ModuleNameClash`] if two IR modules would get the same Rust
    ///   module name.
    pub fn generate(&self, ir: &Ir) -> Result<Bindings, Error> {
        ir.validate().map_err(Error::InvalidIr)?;

        let mut overrides: HashMap<&str, &str> = HashMap::new();
        for (ir_name, rust_name) in &self.module_names {
            if !ir.modules.iter().any(|module| module.name == *ir_name) {
                return Err(Error::UnknownModule {
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
            .map(|(module, rust_name)| emit::module(module, rust_name, &mut warnings));
        let source = format::render(quote::quote! { #(#items)* });
        Ok(Bindings { source, warnings })
    }
}

/// Generated bindings: Rust source plus the warnings raised while generating
/// it.
///
/// Returned by [`Generator::generate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindings {
    source: String,
    warnings: Vec<Warning>,
}

impl Bindings {
    /// The generated Rust source, formatted.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Consumes the bindings, returning the generated Rust source.
    pub fn into_source(self) -> String {
        self.source
    }

    /// Everything the generator skipped or simplified, in the order found.
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }
}
