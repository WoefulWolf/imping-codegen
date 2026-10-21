//! Generates bindings from imping-codegen's x64 fixture IR into `OUT_DIR`, as a
//! mod's build script would.

use std::path::PathBuf;

fn main() {
    let ir_path = "../imping-codegen/tests/fixtures/types.json";
    println!("cargo:rerun-if-changed={ir_path}");
    let text = std::fs::read_to_string(ir_path).expect("fixture IR is readable");
    let ir = imping_ir::Ir::from_json_str(&text).expect("fixture IR parses");
    let bindings = imping_codegen::Generator::new()
        .generate(&ir)
        .expect("fixture IR generates");
    // The fixture triggers warnings on purpose; imping-codegen's tests check
    // them.
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set"));
    std::fs::write(out_dir.join("imped.rs"), bindings.source()).expect("OUT_DIR is writable");
}
