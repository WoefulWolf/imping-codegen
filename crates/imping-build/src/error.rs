//! The error type.

use core::fmt;
use std::path::PathBuf;

/// Why generating bindings failed. Messages name the file involved.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The IR file could not be read.
    Read {
        /// The IR file.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
    /// The IR file is not valid IR.
    Parse {
        /// The IR file.
        path: PathBuf,
        /// Why it could not be parsed.
        source: imping_ir::Error,
    },
    /// Bindings could not be generated from the IR.
    Generate {
        /// The IR file.
        path: PathBuf,
        /// Why generation failed.
        source: imping_codegen::Error,
    },
    /// The bindings could not be written.
    Write {
        /// The file being written.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
    /// [`Builder::generate`](crate::Builder::generate) was called outside a
    /// build script, where `OUT_DIR` is not set.
    OutDirNotSet,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Read { path, source } => {
                write!(f, "failed to read IR file `{}`: {source}", path.display())
            }
            Error::Parse { path, source } => {
                write!(f, "`{}` is not a valid IR file: {source}", path.display())?;
                // The IR error's own cause (e.g. the JSON line and column) is
                // what the user needs to fix the file.
                let mut cause = core::error::Error::source(source);
                while let Some(error) = cause {
                    write!(f, ": {error}")?;
                    cause = error.source();
                }
                Ok(())
            }
            Error::Generate { path, source } => write!(
                f,
                "failed to generate bindings from `{}`: {source}",
                path.display()
            ),
            Error::Write { path, source } => {
                write!(f, "failed to write `{}`: {source}", path.display())
            }
            Error::OutDirNotSet => f.write_str(
                "OUT_DIR is not set; `generate()` is for build.rs, use `generate_to(path)` elsewhere",
            ),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Error::Read { source, .. } | Error::Write { source, .. } => Some(source),
            Error::Parse { source, .. } => Some(source),
            Error::Generate { source, .. } => Some(source),
            Error::OutDirNotSet => None,
        }
    }
}
