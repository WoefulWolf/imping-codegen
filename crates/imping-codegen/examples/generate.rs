//! Generates one set of bindings from the IR files of one or more builds and
//! prints them, with warnings on stderr.
//!
//! Run with: `cargo run -p imping-codegen --example generate -- <label>=<ir.json>...`
//!
//! Without arguments it uses the crate's two-build test fixtures.

use imping_codegen::Generator;
use imping_ir::Ir;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut builds: Vec<(String, String)> = Vec::new();
    for arg in std::env::args().skip(1) {
        let Some((label, path)) = arg.split_once('=') else {
            eprintln!("error: expected `<label>=<ir.json>`, got `{arg}`");
            return ExitCode::FAILURE;
        };
        builds.push((label.to_owned(), path.to_owned()));
    }
    if builds.is_empty() {
        let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        builds.push((
            "steam_2021".to_owned(),
            format!("{fixtures}/builds_2021.json"),
        ));
        builds.push((
            "steam_2026".to_owned(),
            format!("{fixtures}/builds_2026.json"),
        ));
    }

    let mut irs = Vec::with_capacity(builds.len());
    for (_, path) in &builds {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("error: failed to read `{path}`: {error}");
                return ExitCode::FAILURE;
            }
        };
        match Ir::from_json_str(&text) {
            Ok(ir) => irs.push(ir),
            Err(error) => {
                eprintln!("error: `{path}` is not a valid IR file: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    let labelled: Vec<(&str, &Ir)> = builds
        .iter()
        .zip(&irs)
        .map(|((label, _), ir)| (label.as_str(), ir))
        .collect();

    match Generator::new().generate(&labelled) {
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
