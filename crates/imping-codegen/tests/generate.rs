//! Tests generation from fixture IR: output snapshots, warnings and errors.
//!
//! The `codegen-fixture` crate also compiles and runs the x64 output.

use imping_codegen::{Error, Generator, Warning};
use imping_ir::Ir;

fn load(name: &str) -> Ir {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    Ir::from_json_str(&text).unwrap_or_else(|e| panic!("parsing {path}: {e}"))
}

/// Returns a one-module x64 IR with the given types JSON.
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
    let bindings = Generator::new()
        .generate(&[("steam", &load("types.json"))])
        .unwrap();
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
    let bindings = Generator::new()
        .generate(&[("steam", &load("x86.json"))])
        .unwrap();
    insta::assert_snapshot!(bindings.source());
    assert!(bindings.warnings().is_empty());
}

#[test]
fn output_is_deterministic() {
    let ir = load("types.json");
    let first = Generator::new().generate(&[("steam", &ir)]).unwrap();
    let second = Generator::new().generate(&[("steam", &ir)]).unwrap();
    assert_eq!(first, second);
}

#[test]
fn doc_text_has_no_runs_of_spaces() {
    for fixture in ["types.json", "x86.json"] {
        let source = Generator::new()
            .generate(&[("steam", &load(fixture))])
            .unwrap()
            .into_source();
        for line in source.lines() {
            let line = line.trim_start();
            if let Some(text) = line.strip_prefix("///").or(line.strip_prefix("//!")) {
                assert!(!text.trim().contains("  "), "{fixture}: {line}");
            }
        }
    }
}

#[test]
fn module_name_overrides_the_derived_name() {
    let ir = load("x86.json");
    let derived = Generator::new().generate(&[("steam", &ir)]).unwrap();
    assert!(derived.source().contains("pub mod game32 {"));

    let renamed = Generator::new()
        .module_name("Game32.exe", "old_game")
        .generate(&[("steam", &ir)])
        .unwrap();
    assert!(renamed.source().contains("pub mod old_game {"));
    assert!(!renamed.source().contains("pub mod game32 {"));
}

#[test]
fn accept_build_lists_extra_builds_after_the_ir_build() {
    let ir = load("x86.json");
    let bindings = Generator::new()
        .accept_build("steam", "Game32.exe", 0x5f1e2d3c, 0x2b0000)
        // The IR's own build is not repeated.
        .accept_build("steam", "Game32.exe", 0x5f1e2d3c, 0x2a0000)
        .generate(&[("steam", &ir)])
        .unwrap();
    let source = bindings.source();
    let own = source.find("image_size: 0x2a0000").expect("the IR's build");
    let extra = source
        .find("image_size: 0x2b0000")
        .expect("the extra build");
    assert!(own < extra, "the IR's build comes first");
    assert_eq!(source.matches("image_size: 0x2a0000").count(), 1);
    assert_eq!(source.matches("::imping_runtime::Build {").count(), 2);
}

#[test]
fn modules_can_be_initialized_whatever_their_build() {
    let ir = load("x86.json");
    let source = Generator::new()
        .generate(&[("steam", &ir)])
        .unwrap()
        .into_source();
    assert!(source.contains("pub unsafe fn init_any_build() -> ::core::result::Result<"));
    assert!(source.contains("unsafe { MODULE.init_any_build() }"));
}

#[test]
fn rejects_an_extra_build_of_an_unknown_module() {
    let ir = module_with_types("game.exe", "[]");
    let error = Generator::new()
        .accept_build("steam", "other.dll", 1, 2)
        .generate(&[("steam", &ir)])
        .unwrap_err();
    assert!(matches!(
        &error,
        Error::UnknownModule { option: "accept_build", name } if name == "other.dll"
    ));
    assert_eq!(
        error.to_string(),
        "`accept_build` names `other.dll`, which is not in the IR"
    );
}

#[test]
fn rejects_invalid_ir() {
    let ir = module_with_types(
        "game.exe",
        r#"[{ "id": 0, "name": "P", "kind": "pointer", "pointee": 99 }]"#,
    );
    let error = Generator::new().generate(&[("steam", &ir)]).unwrap_err();
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
        .generate(&[("steam", &ir)])
        .unwrap_err();
    assert!(matches!(
        &error,
        Error::UnknownModule { option: "module_name", name } if name == "other.dll"
    ));
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
            .generate(&[("steam", &ir)])
            .unwrap_err();
        assert!(
            matches!(&error, Error::InvalidModuleName { name } if name == bad),
            "{bad}: {error:?}"
        );
    }
    let error = Generator::new()
        .module_name("game.exe", "2game")
        .generate(&[("steam", &ir)])
        .unwrap_err();
    assert_eq!(error.to_string(), "`2game` is not a valid Rust module name");
}

#[test]
fn rejects_a_module_name_clash_until_renamed() {
    let mut ir = module_with_types("game.exe", "[]");
    let mut second = ir.modules[0].clone();
    second.name = "game.dll".to_owned();
    ir.modules.push(second);

    let error = Generator::new().generate(&[("steam", &ir)]).unwrap_err();
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
        .generate(&[("steam", &ir)])
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

    let bindings = Generator::new().generate(&[("steam", &ir)]).unwrap();
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
    let bindings = Generator::new().generate(&[("steam", &ir)]).unwrap();
    assert!(
        bindings
            .source()
            .starts_with("// Generated by imping-codegen ")
    );
    assert!(bindings.warnings().is_empty());
}

fn two_builds() -> (Ir, Ir) {
    (load("builds_2021.json"), load("builds_2026.json"))
}

#[test]
fn builds_x64() {
    let (old, new) = two_builds();
    let bindings = Generator::new()
        .accept_build("steam_2021", "game.exe", 0x60d3a8d2, 0x18f7000)
        .generate(&[("steam_2021", &old), ("steam_2026", &new)])
        .unwrap();
    insta::assert_snapshot!(bindings.source());
    assert!(bindings.warnings().is_empty(), "{:?}", bindings.warnings());
}

#[test]
fn builds_keep_the_order_given() {
    let (old, new) = two_builds();
    let source = Generator::new()
        .generate(&[("steam_2026", &new), ("steam_2021", &old)])
        .unwrap()
        .into_source();
    let first = source.find("Steam2026,").expect("the 2026 variant");
    let second = source.find("Steam2021,").expect("the 2021 variant");
    assert!(first < second, "variants follow the order given");
    assert!(source.contains("pub const RVA_STEAM_2026: u32 = 0x1010;"));
    assert!(source.contains("pub const RVA_STEAM_2021: u32 = 0x1000;"));
}

/// Returns a one-module x64 IR with the given module fields.
fn module_with(fields: &str) -> Ir {
    Ir::from_json_str(&format!(
        r#"{{
            "format_version": {{ "major": 1, "minor": 0 }},
            "modules": [{{
                "name": "game.exe", "architecture": "x64",
                "image_base": "0x140000000", "timestamp": "0x1", "image_size": "0x1000",
                {fields}
            }}]
        }}"#
    ))
    .expect("test IR parses")
}

const INT_AND_FLOAT: &str = r#"
    { "id": 0, "name": "int", "kind": "primitive", "primitive": "i32" },
    { "id": 1, "name": "float", "kind": "primitive", "primitive": "f32" }"#;

/// Generates `old` and `new` together, expecting `BuildsDiffer`, and returns
/// its item and difference.
fn builds_differ(old: &Ir, new: &Ir) -> (String, String) {
    let error = Generator::new()
        .generate(&[("old", old), ("new", new)])
        .unwrap_err();
    let Error::BuildsDiffer(differ) = &error else {
        panic!("expected BuildsDiffer, got {error:?}");
    };
    assert_eq!(
        (
            differ.module.as_str(),
            differ.first.as_str(),
            differ.second.as_str()
        ),
        ("game.exe", "old", "new")
    );
    assert_eq!(differ.to_string(), error.to_string());
    (differ.item.clone(), differ.difference.clone())
}

#[test]
fn rejects_builds_that_differ_on_a_type() {
    let old = module_with(&format!(
        r#""types": [{INT_AND_FLOAT},
            {{ "id": 2, "name": "Player", "kind": "struct", "size": 4, "align": 4,
               "fields": [{{ "name": "health", "type": 0, "offset": 0 }}] }}]"#
    ));
    let new = module_with(&format!(
        r#""types": [{INT_AND_FLOAT},
            {{ "id": 2, "name": "Player", "kind": "struct", "size": 8, "align": 4,
               "fields": [{{ "name": "health", "type": 0, "offset": 0 }},
                          {{ "name": "speed", "type": 1, "offset": 4 }}] }}]"#
    ));
    let (item, difference) = builds_differ(&old, &new);
    assert_eq!(item, "type `Player`");
    assert!(difference.contains("size"), "{difference}");
}

#[test]
fn pointer_types_match_by_what_they_point_to() {
    // A self-referencing struct whose pointer type is named and ordered
    // differently in the second build, as Ghidra exports can be.
    let old = module_with(
        r#""types": [
            { "id": 0, "name": "Node", "kind": "struct", "size": 8, "align": 8,
              "fields": [{ "name": "next", "type": 1, "offset": 0 }] },
            { "id": 1, "name": "Node *", "kind": "pointer", "pointee": 0 }
        ]"#,
    );
    let new = module_with(
        r#""types": [
            { "id": 0, "name": "Node *", "kind": "pointer", "pointee": 2 },
            { "id": 1, "name": "PNODE", "kind": "pointer", "pointee": 2 },
            { "id": 2, "name": "Node", "kind": "struct", "size": 8, "align": 8,
              "fields": [{ "name": "next", "type": 1, "offset": 0 }] }
        ]"#,
    );
    let bindings = Generator::new()
        .generate(&[("old", &old), ("new", &new)])
        .unwrap();
    assert!(bindings.source().contains("pub next: *mut Node,"));
}

#[test]
fn duplicate_pointer_types_count_as_one() {
    // The first build has `void *` twice, as Ghidra's `void *` and `void *64`;
    // the second only once.
    let old = module_with(
        r#""types": [
            { "id": 0, "name": "void", "kind": "primitive", "primitive": "void" },
            { "id": 1, "name": "void *", "kind": "pointer", "pointee": 0 },
            { "id": 2, "name": "void *64", "kind": "pointer", "pointee": 0 },
            { "id": 3, "name": "void *64 *64", "kind": "pointer", "pointee": 2 },
            { "id": 4, "name": "Texture", "kind": "struct", "size": 8, "align": 8,
              "fields": [{ "name": "vt", "type": 3, "offset": 0 }] }
        ],
        "globals": [{ "name": "gData", "rva": "0x2000", "type": 2 }]"#,
    );
    let new = module_with(
        r#""types": [
            { "id": 0, "name": "void", "kind": "primitive", "primitive": "void" },
            { "id": 1, "name": "void *", "kind": "pointer", "pointee": 0 },
            { "id": 2, "name": "void * *", "kind": "pointer", "pointee": 1 },
            { "id": 3, "name": "Texture", "kind": "struct", "size": 8, "align": 8,
              "fields": [{ "name": "vt", "type": 2, "offset": 0 }] }
        ],
        "globals": [{ "name": "gData", "rva": "0x2010", "type": 1 }]"#,
    );
    Generator::new()
        .generate(&[("old", &old), ("new", &new)])
        .unwrap();
}

#[test]
fn rejects_builds_that_differ_on_a_signature() {
    let function = |parameter: u32| {
        module_with(&format!(
            r#""types": [{INT_AND_FLOAT}],
            "functions": [{{ "name": "Tick", "namespace": ["World"], "rva": "0x1000",
                "calling_convention": "win64", "return_type": 0,
                "parameters": [{{ "name": "dt", "type": {parameter},
                                  "storage": {{ "kind": "register", "value": "rcx" }} }}] }}]"#
        ))
    };
    let (item, difference) = builds_differ(&function(1), &function(0));
    assert_eq!(item, "function `World::Tick`");
    assert_eq!(difference, "parameter `dt`: type `float` vs `int`");
}

fn speed_global(type_id: u32) -> Ir {
    module_with(&format!(
        r#""types": [{INT_AND_FLOAT}],
        "globals": [{{ "name": "gSpeed", "rva": "0x2000", "type": {type_id} }}]"#
    ))
}

#[test]
fn rejects_builds_that_differ_on_a_global_type() {
    let (item, difference) = builds_differ(&speed_global(1), &speed_global(0));
    assert_eq!(item, "global `gSpeed`");
    assert!(
        difference.contains("float") && difference.contains("int"),
        "{difference}"
    );
}

#[test]
fn builds_differ_message_names_both_builds() {
    let error = Generator::new()
        .generate(&[("old", &speed_global(1)), ("new", &speed_global(0))])
        .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("`game.exe`: global `gSpeed` differs between builds `old` and `new`: "),
        "{error}"
    );
}

#[test]
fn rejects_builds_of_different_architectures() {
    let (old, mut new) = two_builds();
    new.modules[0].architecture = imping_ir::Architecture::X86;
    let error = Generator::new()
        .generate(&[("steam_2021", &old), ("steam_2026", &new)])
        .unwrap_err();
    assert!(
        matches!(&error, Error::BuildsDiffer(differ) if differ.item == "the module"),
        "{error:?}"
    );
}

#[test]
fn rejects_invalid_build_labels() {
    let ir = module_with_types("game.exe", "[]");
    for bad in [
        "Steam",
        "2021",
        "steam__2021",
        "steam_",
        "_steam",
        "steam-2021",
        "",
    ] {
        let error = Generator::new().generate(&[(bad, &ir)]).unwrap_err();
        assert!(
            matches!(&error, Error::InvalidBuildLabel { label } if label == bad),
            "{bad}: {error:?}"
        );
    }
    let error = Generator::new().generate(&[("Steam", &ir)]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "`Steam` is not a valid build label; use lowercase letters, digits and single \
         underscores, starting with a letter, as in `steam_2021`"
    );
}

#[test]
fn rejects_duplicate_builds() {
    let ir = module_with_types("game.exe", "[]");
    for (first, second) in [("steam", "steam"), ("steam_2021", "steam2021")] {
        let error = Generator::new()
            .generate(&[(first, &ir), (second, &ir)])
            .unwrap_err();
        assert!(
            matches!(&error, Error::DuplicateBuild { label } if label == second),
            "{second}: {error:?}"
        );
    }
}

#[test]
fn rejects_an_extra_image_of_an_unknown_build() {
    let ir = module_with_types("game.exe", "[]");
    let error = Generator::new()
        .accept_build("gog", "game.exe", 1, 2)
        .generate(&[("steam", &ir)])
        .unwrap_err();
    assert!(
        matches!(&error, Error::UnknownBuild { label } if label == "gog"),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "`accept_build` names build `gog`, which no IR has"
    );
}

#[test]
fn rejects_an_extra_image_of_a_module_its_build_lacks() {
    let game = module_with_types("game.exe", "[]");
    let other = module_with_types("other.dll", "[]");
    let error = Generator::new()
        .accept_build("steam", "other.dll", 1, 2)
        .generate(&[("steam", &game), ("gog", &other)])
        .unwrap_err();
    assert!(
        matches!(
            &error,
            Error::UnknownModule { option: "accept_build", name } if name == "other.dll"
        ),
        "{error:?}"
    );
}

#[test]
fn a_module_only_one_build_has_lists_only_that_build() {
    let game = module_with_types("game.exe", "[]");
    let other = module_with_types("other.dll", "[]");
    let source = Generator::new()
        .generate(&[("steam", &game), ("gog", &other)])
        .unwrap()
        .into_source();
    assert!(source.contains("pub mod game {"));
    assert!(source.contains("pub mod other {"));
    assert_eq!(source.matches("pub enum Build {").count(), 2);
}

#[test]
fn no_builds_generate_only_the_header() {
    let bindings = Generator::new().generate(&[]).unwrap();
    assert!(!bindings.source().contains("pub mod"));
    assert!(bindings.warnings().is_empty());
}
