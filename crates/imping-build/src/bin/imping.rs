//! The `imping` command-line tool, which generates Rust bindings from IR files.
//!
//! Run `imping --help` for usage.

fn main() -> std::process::ExitCode {
    imping_build::__cli::main()
}
