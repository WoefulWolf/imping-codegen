//! Generation from fixture IR: snapshots of the output, warnings and errors.
//!
//! The x64 output is also compiled and exercised by the `codegen-fixture`
//! crate in this workspace.

use imping_codegen::{Error, Generator, Warning};
use imping_ir::Ir;

fn load(name: &str) -> Ir {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    Ir::from_json_str(&text).unwrap_or_else(|e| panic!("parsing {path}: {e}"))
}

/// A one-module x64 IR with the given types JSON.
fn module_with_types(name: &str, types: &str) -> Ir {
    Ir::from_json_str(&format!(
        r#"{{
            "format_version": {{ "major": 1, "minor": 0 }},
            "modules": [{{
                "name": "{name}", "architecture": "x64",
                "image_base": "0x140000000", "timestamp": "0x1", "image_size": "0x1000",
                "types": {types}
            }}]
        }}"#
    ))
    .expect("test IR parses")
}

#[test]
fn types_x64() {
    let bindings = Generator::new().generate(&load("types.json")).unwrap();
    insta::assert_snapshot!(bindings.source());
    let warnings: Vec<String> = bindings
        .warnings()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        warnings,
        [
            "`game.exe` enum `SmallSigned` constant `Huge` = 1000 does not fit the underlying \
             type and was truncated",
            "`game.exe` type `Broken` emitted as opaque bytes: field `b` overlaps `a`",
            "`game.exe` type `Unsized` emitted as opaque bytes: field `r` has no known size",
            "`game.exe` type `LoopA` emitted as opaque bytes: it is part of a typedef cycle",
            "`game.exe` type `LoopB` emitted as opaque bytes: it is part of a typedef cycle",
            "`game.exe` function `util::FastPath` uses custom storage; only its RVA is generated",
            "`game.exe` function `util::TakesRenderer` skipped: parameter `r` has no known size",
            "`game.exe` function `util::Mixed` is tagged `imping-safe` but its call stays unsafe: \
             parameter `type` is an array",
            "`game.exe` function `util::Release` is tagged `imping-safe` but its call stays \
             unsafe: parameter `memory` is a `void` pointer",
            "`game.exe` function `util::Store` is tagged `imping-safe` but its call stays unsafe: \
             parameter `holder` is passed by value and contains pointers",
            "`game.exe` vtable `game::Entity` skipped; vtables are not supported yet",
        ]
    );
    assert_eq!(
        bindings.warnings()[0],
        Warning::EnumValueTruncated {
            module: "game.exe".to_owned(),
            enum_name: "SmallSigned".to_owned(),
            variant: "Huge".to_owned(),
            value: 1000,
        }
    );
    assert_eq!(
        bindings.warnings()[1],
        Warning::TypeMadeOpaque {
            module: "game.exe".to_owned(),
            type_name: "Broken".to_owned(),
            reason: "field `b` overlaps `a`".to_owned(),
        }
    );
}

#[test]
fn types_x86() {
    let bindings = Generator::new().generate(&load("x86.json")).unwrap();
    insta::assert_snapshot!(bindings.source());
    assert!(bindings.warnings().is_empty());
}

#[test]
fn output_is_deterministic() {
    let ir = load("types.json");
    let first = Generator::new().generate(&ir).unwrap();
    let second = Generator::new().generate(&ir).unwrap();
    assert_eq!(first, second);
}

#[test]
fn module_name_overrides_the_derived_name() {
    let ir = load("x86.json");
    let derived = Generator::new().generate(&ir).unwrap();
    assert!(derived.source().contains("pub mod game32 {"));

    let renamed = Generator::new()
        .module_name("Game32.exe", "old_game")
        .generate(&ir)
        .unwrap();
    assert!(renamed.source().contains("pub mod old_game {"));
    assert!(!renamed.source().contains("pub mod game32 {"));
}

#[test]
fn rejects_invalid_ir() {
    let ir = module_with_types(
        "game.exe",
        r#"[{ "id": 0, "name": "P", "kind": "pointer", "pointee": 99 }]"#,
    );
    let error = Generator::new().generate(&ir).unwrap_err();
    assert!(matches!(error, Error::InvalidIr(_)), "{error:?}");
    assert!(
        error.to_string().starts_with("the IR is invalid: "),
        "{error}"
    );
}

#[test]
fn rejects_a_rename_of_an_unknown_module() {
    let ir = module_with_types("game.exe", "[]");
    let error = Generator::new()
        .module_name("other.dll", "other")
        .generate(&ir)
        .unwrap_err();
    assert!(matches!(&error, Error::UnknownModule { name } if name == "other.dll"));
    assert_eq!(
        error.to_string(),
        "`module_name` names `other.dll`, which is not in the IR"
    );
}

#[test]
fn rejects_an_invalid_module_name() {
    let ir = module_with_types("game.exe", "[]");
    for bad in ["2game", "type", "self", "game-1", ""] {
        let error = Generator::new()
            .module_name("game.exe", bad)
            .generate(&ir)
            .unwrap_err();
        assert!(
            matches!(&error, Error::InvalidModuleName { name } if name == bad),
            "{bad}: {error:?}"
        );
    }
    let error = Generator::new()
        .module_name("game.exe", "2game")
        .generate(&ir)
        .unwrap_err();
    assert_eq!(error.to_string(), "`2game` is not a valid Rust module name");
}

#[test]
fn rejects_a_module_name_clash_until_renamed() {
    let mut ir = module_with_types("game.exe", "[]");
    let mut second = ir.modules[0].clone();
    second.name = "game.dll".to_owned();
    ir.modules.push(second);

    let error = Generator::new().generate(&ir).unwrap_err();
    assert!(matches!(
        &error,
        Error::ModuleNameClash { first, second, rust_name }
            if first == "game.exe" && second == "game.dll" && rust_name == "game"
    ));
    assert_eq!(
        error.to_string(),
        "IR modules `game.exe` and `game.dll` both become Rust module `game`; rename one"
    );

    let bindings = Generator::new()
        .module_name("game.dll", "game_dll")
        .generate(&ir)
        .unwrap();
    assert!(bindings.source().contains("pub mod game {"));
    assert!(bindings.source().contains("pub mod game_dll {"));
}

#[test]
fn functions_with_too_many_parameters_are_callable_but_not_hookable() {
    let parameters: Vec<String> = (0..15)
        .map(|i| {
            format!(r#"{{ "name": "p{i}", "type": 0, "storage": {{ "kind": "stack", "offset": {i} }} }}"#)
        })
        .collect();
    let ir = Ir::from_json_str(&format!(
        r#"{{
            "format_version": {{ "major": 1, "minor": 0 }},
            "modules": [{{
                "name": "game.exe", "architecture": "x64",
                "image_base": "0x140000000", "timestamp": "0x1", "image_size": "0x1000",
                "types": [{{ "id": 0, "name": "int", "kind": "primitive", "primitive": "i32" }}],
                "functions": [{{
                    "name": "Wide", "namespace": ["util"], "rva": "0x1000",
                    "calling_convention": "win64", "return_type": 0,
                    "parameters": [{}]
                }}]
            }}]
        }}"#,
        parameters.join(", ")
    ))
    .expect("test IR parses");

    let bindings = Generator::new().generate(&ir).unwrap();
    assert_eq!(
        bindings.warnings(),
        [Warning::HookUnavailable {
            module: "game.exe".to_owned(),
            function: "util::Wide".to_owned(),
            reason: "it has 15 parameters, more than the 14 the hooking engine supports".to_owned(),
        }]
    );
    assert_eq!(
        bindings.warnings()[0].to_string(),
        "`game.exe` function `util::Wide` can be called but not hooked: it has 15 parameters, \
         more than the 14 the hooking engine supports"
    );
    let source = bindings.source();
    assert!(
        source.contains("pub unsafe fn Wide("),
        "the call wrapper exists"
    );
    assert!(source.contains("pub fn ptr() -> Signature"));
    assert!(!source.contains("pub fn hook("), "but no hook");
    assert!(!source.contains("static SLOT"));
}

#[test]
fn an_empty_ir_generates_only_the_header() {
    let ir =
        Ir::from_json_str(r#"{ "format_version": { "major": 1, "minor": 0 }, "modules": [] }"#)
            .unwrap();
    let bindings = Generator::new().generate(&ir).unwrap();
    assert!(
        bindings
            .source()
            .starts_with("// Generated by imping-codegen ")
    );
    assert!(bindings.warnings().is_empty());
}
