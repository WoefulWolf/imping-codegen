# imping-codegen

Rust code generation for the imping modding suite. This repository holds two crates:

- `imping-codegen` turns an IR file's contents (`imping-ir`) into Rust source: `#[repr(C)]` structs with layout assertions, integer-newtype enums, and typed functions and globals that call into `imping-runtime`.
- `imping-build` wraps it for use from a `build.rs` and from the command line.

From a build script:

```rust
// build.rs
fn main() -> Result<(), imping_build::Error> {
    imping_build::Builder::new("ir/game.json").generate()
}
```

```rust
// src/lib.rs
include!(concat!(env!("OUT_DIR"), "/imped.rs"));
```

From the command line, install the `imping` tool with the `cli` feature:

```powershell
cargo install --path crates/imping-build --features cli
imping generate ir/game.json -o src/imped.rs --module-name GameAssembly.dll=game
imping check ir/game.json
```

The generated code depends on the `imping-runtime` crate.

Status: early development. Not published.

## Development

The crates fetch `imping-ir` (and, for tests, `imping-runtime`) from GitHub, or use the local checkouts inside a clone of the [imping](https://github.com/WoefulWolf/imping) repository.

`crates/codegen-fixture` is a test-only crate: its build script generates bindings from `imping-codegen`'s x64 test fixture, and its tests compile and exercise them. Snapshot tests use [insta](https://insta.rs); after an intended change to the output, review the new snapshots and accept them (for example with `cargo insta review`, or by running the tests with `INSTA_UPDATE=always`).

Run all checks (formatting, clippy, tests, docs) from the repo root:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/check.ps1
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

AI assistance was used for parts of this project.
