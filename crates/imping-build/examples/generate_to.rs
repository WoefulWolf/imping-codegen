//! Generates bindings from an IR file into a file, printing any warnings.
//!
//! Run with: `cargo run -p imping-build --example generate_to -- [<ir.json> [<output.rs>]]`
//!
//! Without arguments it uses the crate's test fixture and writes `imped.rs`
//! in the system's temporary directory.

use imping_build::Builder;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let ir = args.next().map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/small.json")
    });
    let output = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("imped.rs"));

    match Builder::new(&ir).generate_to(&output) {
        Ok(warnings) => {
            for warning in &warnings {
                eprintln!("warning: {warning}");
            }
            println!("wrote {}", output.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
