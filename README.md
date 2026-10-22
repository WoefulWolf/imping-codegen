# imping-codegen

Typed Rust bindings from imping IR: `#[repr(C)]` structs with layout checks, integer-newtype enums, and functions and globals you can call and hook. `imping-codegen` generates the source, and `imping-build` runs it from a build script or the `imping` CLI.

```toml
[dependencies]
imping-runtime = { git = "https://github.com/WoefulWolf/imping-runtime" }

[build-dependencies]
imping-build = { git = "https://github.com/WoefulWolf/imping-codegen" }
```

```rust
// build.rs
fn main() -> Result<(), imping_build::Error> {
    imping_build::Builder::new("target.ir.json").generate()
}
```

```rust
// src/lib.rs
include!(concat!(env!("OUT_DIR"), "/imped.rs"));

pub fn start() -> imping_runtime::Result<()> {
    unsafe { target::init() }?;
    let hook = target::SetVolume::hook(|original, level| {
        println!("volume set to {level}");
        original.forward()
    })?;
    hook.enable()?;
    hook.leak(); // keep it for the rest of the process
    Ok(())
}
```

Or from the command line:

```sh
cargo install imping-build --git https://github.com/WoefulWolf/imping-codegen --features cli
imping generate target.ir.json -o src/imped.rs
```

[API docs](https://woefulwolf.github.io/imping-codegen/) · Early development.

If imping helped your project, a mention or link back is appreciated. I'd also love to see what you make with it: share it in [Show and tell](https://github.com/WoefulWolf/imping/discussions/categories/show-and-tell), and it might get featured here!

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.

AI assistance was used for parts of this project.
