//! The `imping` command-line tool. Not a supported API: it is public only so
//! the `imping` binary can call it.

use crate::{Builder, Error};
use clap::{Parser, Subcommand};
use imping_codegen::Warning;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

/// Generates Rust bindings from imping IR files.
#[derive(Debug, Parser)]
#[command(name = "imping", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generates Rust bindings from an IR file.
    Generate {
        /// The IR file to read.
        ir: PathBuf,
        /// Where to write the bindings; standard output if omitted.
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
        #[command(flatten)]
        names: ModuleNames,
    },
    /// Checks that an IR file generates bindings, and prints any warnings.
    Check {
        /// The IR file to read.
        ir: PathBuf,
        #[command(flatten)]
        names: ModuleNames,
    },
}

#[derive(Debug, clap::Args)]
struct ModuleNames {
    /// Names the Rust module generated for an IR module, e.g.
    /// `GameAssembly.dll=game`. May be repeated.
    #[arg(long = "module-name", value_name = "IR=RUST", value_parser = parse_module_name)]
    module_names: Vec<(String, String)>,
}

fn parse_module_name(value: &str) -> Result<(String, String), String> {
    match value.split_once('=') {
        Some((ir, rust)) if !ir.is_empty() && !rust.is_empty() => {
            Ok((ir.to_owned(), rust.to_owned()))
        }
        _ => Err(format!(
            "expected `<IR module>=<Rust module>`, got `{value}`"
        )),
    }
}

/// Runs the tool with the process's arguments. Exits with 0 on success
/// (warnings included) and 1 on any error.
pub fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            // `--help` and `--version` also arrive here, and are successes.
            return if error.use_stderr() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    match run(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn builder(ir: PathBuf, names: ModuleNames) -> Builder {
    names
        .module_names
        .into_iter()
        .fold(Builder::new(ir), |builder, (ir_name, rust_name)| {
            builder.module_name(ir_name, rust_name)
        })
}

fn print_warnings(warnings: &[Warning]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}

fn run(command: Command) -> Result<(), Error> {
    match command {
        Command::Generate { ir, output, names } => {
            let builder = builder(ir, names);
            let warnings = match output {
                Some(path) => builder.generate_to(path)?,
                None => {
                    let (source, warnings) = builder.render()?;
                    std::io::stdout()
                        .lock()
                        .write_all(source.as_bytes())
                        .map_err(|source| Error::Write {
                            path: PathBuf::from("standard output"),
                            source,
                        })?;
                    warnings
                }
            };
            print_warnings(&warnings);
        }
        Command::Check { ir, names } => {
            let display = ir.display().to_string();
            let (_, warnings) = builder(ir, names).render()?;
            print_warnings(&warnings);
            let plural = if warnings.len() == 1 { "" } else { "s" };
            println!(
                "`{display}` generates bindings ({} warning{plural})",
                warnings.len()
            );
        }
    }
    Ok(())
}
