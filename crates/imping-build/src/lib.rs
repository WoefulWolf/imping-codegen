//! Build-script helper and command-line tool that generate Rust bindings for
//! the imping modding suite.
//!
//! [`Builder`] reads an IR file, generates bindings with `imping-codegen` and
//! writes them, from a `build.rs` or anywhere else.
//!
//! # From a build script
//!
//! ```no_run
//! // build.rs
//! fn main() -> Result<(), imping_build::Error> {
//!     imping_build::Builder::new("ir/game.json").generate()
//! }
//! ```
//!
//! [`Builder::generate`] writes `$OUT_DIR/imped.rs`, which the crate then
//! includes:
//!
//! ```ignore
//! include!(concat!(env!("OUT_DIR"), "/imped.rs"));
//! ```
//!
//! # From the command line
//!
//! With the `cli` feature, this crate also builds the `imping` tool:
//!
//! ```text
//! cargo install --path crates/imping-build --features cli
//! imping generate ir/game.json -o src/imped.rs --module-name GameAssembly.dll=game
//! imping check ir/game.json
//! ```

mod error;

#[cfg(feature = "cli")]
#[doc(hidden)]
pub mod __cli;

pub use crate::error::Error;

use imping_codegen::{Generator, Warning};
use imping_ir::Ir;
use std::path::{Path, PathBuf};

/// The default name of the file [`Builder::generate`] writes to `OUT_DIR`.
const DEFAULT_OUT_FILE: &str = "imped.rs";

/// Generates bindings from one IR file.
///
/// Create one with [`Builder::new`], optionally configure it, then call
/// [`generate`](Self::generate) from a build script or
/// [`generate_to`](Self::generate_to) anywhere else.
#[derive(Debug, Clone)]
pub struct Builder {
    ir_path: PathBuf,
    generator: Generator,
    out_file: PathBuf,
}

impl Builder {
    /// A builder for the IR file at `ir_path`.
    ///
    /// In a build script, a relative path is relative to the crate's root.
    pub fn new(ir_path: impl Into<PathBuf>) -> Self {
        Self {
            ir_path: ir_path.into(),
            generator: Generator::new(),
            out_file: PathBuf::from(DEFAULT_OUT_FILE),
        }
    }

    /// Names the Rust module generated for the IR module `ir_name`, instead of
    /// the name derived from its file name. See
    /// [`imping_codegen::Generator::module_name`].
    pub fn module_name(mut self, ir_name: impl Into<String>, rust_name: impl Into<String>) -> Self {
        self.generator = self.generator.module_name(ir_name, rust_name);
        self
    }

    /// Names the file [`generate`](Self::generate) writes in `OUT_DIR`;
    /// `imped.rs` by default. Use it to keep the bindings of several IR files
    /// apart.
    pub fn out_file(mut self, name: impl Into<PathBuf>) -> Self {
        self.out_file = name.into();
        self
    }

    /// Generates the bindings into `$OUT_DIR/imped.rs` (or the name set with
    /// [`out_file`](Self::out_file)), for use from a build script.
    ///
    /// Also prints `cargo:rerun-if-changed` for the IR file, so the bindings
    /// are regenerated when it changes, and one `cargo:warning` per
    /// [`Warning`].
    ///
    /// # Errors
    ///
    /// [`Error::OutDirNotSet`] outside a build script, otherwise as for
    /// [`generate_to`](Self::generate_to).
    pub fn generate(&self) -> Result<(), Error> {
        let out_dir = std::env::var_os("OUT_DIR").ok_or(Error::OutDirNotSet)?;
        println!("cargo:rerun-if-changed={}", self.ir_path.display());
        let warnings = self.generate_to(Path::new(&out_dir).join(&self.out_file))?;
        for warning in warnings {
            println!("cargo:warning=imping: {warning}");
        }
        Ok(())
    }

    /// Generates the bindings into the file at `path`, without printing
    /// anything, and returns the warnings.
    ///
    /// # Errors
    ///
    /// [`Error::Read`], [`Error::Parse`], [`Error::Generate`] or
    /// [`Error::Write`], each naming the file involved.
    pub fn generate_to(&self, path: impl AsRef<Path>) -> Result<Vec<Warning>, Error> {
        let path = path.as_ref();
        let (source, warnings) = self.render()?;
        std::fs::write(path, source).map_err(|source| Error::Write {
            path: path.to_owned(),
            source,
        })?;
        Ok(warnings)
    }

    /// Reads the IR and generates the bindings, with the `// Source:` line
    /// added to the header.
    pub(crate) fn render(&self) -> Result<(String, Vec<Warning>), Error> {
        let path = || self.ir_path.clone();
        let text = std::fs::read_to_string(&self.ir_path).map_err(|source| Error::Read {
            path: path(),
            source,
        })?;
        let ir = Ir::from_json_str(&text).map_err(|source| Error::Parse {
            path: path(),
            source,
        })?;
        let bindings = self
            .generator
            .generate(&ir)
            .map_err(|source| Error::Generate {
                path: path(),
                source,
            })?;
        let warnings = bindings.warnings().to_vec();
        let source = with_source_line(bindings.into_source(), &self.ir_path);
        Ok((source, warnings))
    }
}

/// Adds `// Source: <path>` after the generated file's first header line.
fn with_source_line(source: String, ir_path: &Path) -> String {
    let line = format!("// Source: {}\n", ir_path.display());
    match source.find('\n') {
        Some(end) => {
            let mut out = String::with_capacity(source.len() + line.len());
            out.push_str(&source[..=end]);
            out.push_str(&line);
            out.push_str(&source[end + 1..]);
            out
        }
        None => format!("{source}\n{line}"),
    }
}

#[cfg(test)]
mod tests {
    use super::with_source_line;
    use std::path::Path;

    #[test]
    fn the_source_line_follows_the_first_header_line() {
        let source =
            "// Generated by imping-codegen 0.1.0. Do not edit.\n\npub mod game {}\n".to_owned();
        assert_eq!(
            with_source_line(source, Path::new("ir/game.json")),
            "// Generated by imping-codegen 0.1.0. Do not edit.\n// Source: ir/game.json\n\npub mod game {}\n"
        );
    }
}
