//! Generates bindings from an IR file and prints them, with warnings on stderr.
//!
//! Run with: `cargo run -p imping-codegen --example generate -- <ir.json> [<ir module>=<rust module>]`
//!
//! Without arguments it uses the crate's x64 test fixture.

use imping_codegen::Generator;
use imping_ir::Ir;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/types.json").to_owned()
    });

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("error: failed to read `{path}`: {error}");
            return ExitCode::FAILURE;
        }
    };
    let ir = match Ir::from_json_str(&text) {
        Ok(ir) => ir,
        Err(error) => {
            eprintln!("error: `{path}` is not a valid IR file: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut generator = Generator::new();
    for rename in args {
        let Some((ir_name, rust_name)) = rename.split_once('=') else {
            eprintln!("error: expected `<ir module>=<rust module>`, got `{rename}`");
            return ExitCode::FAILURE;
        };
        generator = generator.module_name(ir_name, rust_name);
    }

    match generator.generate(&ir) {
        Ok(bindings) => {
            for warning in bindings.warnings() {
                eprintln!("warning: {warning}");
            }
            print!("{}", bindings.source());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
