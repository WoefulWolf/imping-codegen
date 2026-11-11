//! The `imping` command-line tool.
//!
//! Not a supported API; it's public only so the `imping` binary can call it.

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
    /// Generates Rust bindings from the IR files of one or more builds.
    Generate {
        /// Where to write the bindings; standard output if omitted.
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
        #[command(flatten)]
        options: Options,
    },
    /// Checks that the IR files generate bindings, and prints any warnings.
    Check {
        #[command(flatten)]
        options: Options,
    },
}

#[derive(Debug, clap::Args)]
struct Options {
    /// The IR file of a build, with the label that names the build in the
    /// bindings, e.g. `steam_2021=ir/target-2021.json`. Repeat it for each
    /// build.
    #[arg(long = "ir", value_name = "LABEL=FILE", required = true, value_parser = parse_ir)]
    irs: Vec<(String, PathBuf)>,
    /// Names the Rust module generated for an IR module, e.g.
    /// `TargetCore.dll=target`. May be repeated.
    #[arg(long = "module-name", value_name = "IR=RUST", value_parser = parse_module_name)]
    module_names: Vec<(String, String)>,
    /// Makes an IR module's `init()` also accept another image as the
    /// labelled build, by the timestamp and image size from its image header,
    /// e.g. `steam:target.exe=0x64b8f2a1:0xa3c000`. May be repeated.
    #[arg(long = "accept-build", value_name = "LABEL:IR=TIMESTAMP:IMAGE_SIZE", value_parser = parse_accept_build)]
    accept_builds: Vec<(String, String, u32, u32)>,
}

fn parse_ir(value: &str) -> Result<(String, PathBuf), String> {
    match value.split_once('=') {
        Some((label, path)) if !label.is_empty() && !path.is_empty() => {
            Ok((label.to_owned(), PathBuf::from(path)))
        }
        _ => Err(format!("expected `<label>=<IR file>`, got `{value}`")),
    }
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

fn parse_accept_build(value: &str) -> Result<(String, String, u32, u32), String> {
    let parsed = value.split_once('=').and_then(|(build, image)| {
        let (label, ir) = build.split_once(':')?;
        let (timestamp, image_size) = image.split_once(':')?;
        Some((label, ir, parse_u32(timestamp)?, parse_u32(image_size)?))
    });
    match parsed {
        Some((label, ir, timestamp, image_size)) if !label.is_empty() && !ir.is_empty() => {
            Ok((label.to_owned(), ir.to_owned(), timestamp, image_size))
        }
        _ => Err(format!(
            "expected `<label>:<IR module>=<timestamp>:<image size>` (decimal or 0x hex), got \
             `{value}`"
        )),
    }
}

/// Parses a `u32` in decimal or `0x` hex.
fn parse_u32(value: &str) -> Option<u32> {
    match value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => value.parse().ok(),
    }
}

/// Runs the tool with the process's arguments.
///
/// Exits with 0 on success, warnings included, and 1 on any error.
pub fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            // `--help` and `--version` land here too, and they're successes.
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

fn builder(options: Options) -> Builder {
    let builder = options
        .irs
        .into_iter()
        .fold(Builder::new(), |builder, (label, path)| {
            builder.ir(label, path)
        });
    let builder = options
        .module_names
        .into_iter()
        .fold(builder, |builder, (ir_name, rust_name)| {
            builder.module_name(ir_name, rust_name)
        });
    options.accept_builds.into_iter().fold(
        builder,
        |builder, (label, ir_name, timestamp, image_size)| {
            builder.accept_build(label, ir_name, timestamp, image_size)
        },
    )
}

fn print_warnings(warnings: &[Warning]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}

fn run(command: Command) -> Result<(), Error> {
    match command {
        Command::Generate { output, options } => {
            let builder = builder(options);
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
        Command::Check { options } => {
            let files: Vec<String> = options
                .irs
                .iter()
                .map(|(_, path)| format!("`{}`", path.display()))
                .collect();
            let (_, warnings) = builder(options).render()?;
            print_warnings(&warnings);
            let plural = if warnings.len() == 1 { "" } else { "s" };
            let verb = if files.len() == 1 {
                "generates"
            } else {
                "generate"
            };
            println!(
                "{} {verb} bindings ({} warning{plural})",
                files.join(" and "),
                warnings.len()
            );
        }
    }
    Ok(())
}
