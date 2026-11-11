//! Generates bindings from imping-codegen's x64 fixture IRs into `OUT_DIR`, as
//! a mod's build script would: `imped.rs` from one build, `builds.rs` from two.

use imping_ir::Ir;
use std::path::{Path, PathBuf};

fn load(path: &str) -> Ir {
    println!("cargo:rerun-if-changed={path}");
    let text = std::fs::read_to_string(path).expect("fixture IR is readable");
    Ir::from_json_str(&text).expect("fixture IR parses")
}

fn write(out_dir: &Path, file: &str, source: &str) {
    std::fs::write(out_dir.join(file), source).expect("OUT_DIR is writable");
}

fn main() {
    let fixtures = "../imping-codegen/tests/fixtures";
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set"));

    let types = load(&format!("{fixtures}/types.json"));
    let bindings = imping_codegen::Generator::new()
        .generate(&[("steam", &types)])
        .expect("fixture IR generates");
    // The fixture triggers warnings on purpose; imping-codegen's tests check
    // them.
    write(&out_dir, "imped.rs", bindings.source());

    let old = load(&format!("{fixtures}/builds_2021.json"));
    let new = load(&format!("{fixtures}/builds_2026.json"));
    let bindings = imping_codegen::Generator::new()
        .accept_build("steam_2021", "game.exe", 0x60d3a8d2, 0x18f7000)
        .generate(&[("steam_2021", &old), ("steam_2026", &new)])
        .expect("fixture IRs generate");
    assert!(bindings.warnings().is_empty(), "{:?}", bindings.warnings());
    write(&out_dir, "builds.rs", bindings.source());
}
