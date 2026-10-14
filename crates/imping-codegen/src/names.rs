//! Turning IR names into Rust identifiers.
//!
//! Names are handled as their bare text (`type`, not `r#type`) until they are
//! turned into tokens by [`ident`], which adds the `r#` prefix for keywords.

use proc_macro2::{Ident, Span};
use std::collections::HashSet;

/// Keywords that must be written as raw identifiers (`r#type`), including
/// reserved ones and those of the 2018 and 2024 editions.
const RAW_KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "do", "dyn",
    "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let",
    "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref", "return",
    "static", "struct", "trait", "true", "try", "type", "typeof", "unsafe", "unsized", "use",
    "virtual", "where", "while", "yield",
];

/// Keywords that can't be raw identifiers; they get a trailing underscore.
const UNRAWABLE_KEYWORDS: &[&str] = &["self", "Self", "super", "crate"];

/// Turns a source name into a valid bare identifier: characters other than
/// ASCII letters, digits and `_` become `_`, a leading digit gets a `_`
/// prefix, and keywords that can't be raw get a trailing `_`.
pub(crate) fn sanitize(name: &str) -> String {
    let mut text: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if text.is_empty() || text == "_" {
        return "__".to_owned();
    }
    if text.starts_with(|c: char| c.is_ascii_digit()) {
        text.insert(0, '_');
    }
    if UNRAWABLE_KEYWORDS.contains(&text.as_str()) {
        text.push('_');
    }
    text
}

/// Converts a source name to a snake-case identifier, for module names.
///
/// Words are split at non-alphanumeric characters, at a lowercase letter
/// followed by an uppercase one, and at the end of an acronym
/// (`HTTPClient` -> `http_client`). Digits never start a word, so
/// `D3D11Device` becomes `d3d11_device`.
pub(crate) fn snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !word.is_empty() {
                words.push(core::mem::take(&mut word));
            }
            continue;
        }
        if c.is_ascii_uppercase() && !word.is_empty() {
            let previous = chars[i - 1];
            let next_is_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            let ends_acronym =
                (previous.is_ascii_uppercase() || previous.is_ascii_digit()) && next_is_lower;
            if previous.is_ascii_lowercase() || ends_acronym {
                words.push(core::mem::take(&mut word));
            }
        }
        word.push(c.to_ascii_lowercase());
    }
    if !word.is_empty() {
        words.push(word);
    }
    sanitize(&words.join("_"))
}

/// Derives a Rust module name from an IR module's file name: the extension
/// is dropped and the rest converted to snake case.
pub(crate) fn module_name(file_name: &str) -> String {
    let stem = match file_name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => file_name,
    };
    snake_case(stem)
}

/// Returns whether `text` is an identifier usable without `r#`: ASCII, not a
/// keyword, not `_`.
pub(crate) fn is_plain_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && text != "_"
        && !RAW_KEYWORDS.contains(&text)
        && !UNRAWABLE_KEYWORDS.contains(&text)
}

/// Turns a bare identifier from [`sanitize`] into a token, raw if it is a
/// keyword.
pub(crate) fn ident(text: &str) -> Ident {
    if RAW_KEYWORDS.contains(&text) {
        Ident::new_raw(text, Span::call_site())
    } else {
        Ident::new(text, Span::call_site())
    }
}

/// The names already used in one Rust scope, for resolving clashes.
#[derive(Debug, Default)]
pub(crate) struct Scope {
    used: HashSet<String>,
}

impl Scope {
    /// Marks `name` as taken without claiming it for an item.
    pub(crate) fn reserve(&mut self, name: &str) {
        self.used.insert(name.to_owned());
    }

    /// Claims `preferred` if it is free, otherwise the first free name among
    /// `fallback(2)`, `fallback(3)`, ...
    pub(crate) fn claim(
        &mut self,
        preferred: &str,
        mut fallback: impl FnMut(u32) -> String,
    ) -> String {
        if self.used.insert(preferred.to_owned()) {
            return preferred.to_owned();
        }
        (2..)
            .map(&mut fallback)
            .find(|candidate| self.used.insert(candidate.clone()))
            .expect("an unbounded range always yields a free name")
    }

    /// Claims `preferred`, falling back to `preferred_2`, `preferred_3`, ...
    pub(crate) fn claim_numbered(&mut self, preferred: &str) -> String {
        self.claim(preferred, |n| format!("{preferred}_{n}"))
    }
}

/// Makes a name safe to show inside a doc comment's code span.
pub(crate) fn doc_name(name: &str) -> String {
    name.replace('`', "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_handles_invalid_characters_digits_and_keywords() {
        assert_eq!(sanitize("Vector3"), "Vector3");
        assert_eq!(sanitize("operator=="), "operator__");
        assert_eq!(sanitize("~Entity"), "_Entity");
        assert_eq!(sanitize("<lambda_1>"), "_lambda_1_");
        assert_eq!(sanitize("8bit"), "_8bit");
        assert_eq!(sanitize("self"), "self_");
        assert_eq!(sanitize("Self"), "Self_");
        assert_eq!(sanitize("type"), "type");
        assert_eq!(sanitize(""), "__");
        assert_eq!(sanitize("?"), "__");
        assert_eq!(sanitize("café"), "caf_");
    }

    #[test]
    fn snake_case_splits_words_acronyms_and_keeps_digits() {
        assert_eq!(snake_case("Entity"), "entity");
        assert_eq!(snake_case("CGameWorld"), "c_game_world");
        assert_eq!(snake_case("HTTPClient"), "http_client");
        assert_eq!(snake_case("D3D11Device"), "d3d11_device");
        assert_eq!(snake_case("Vector3D"), "vector3d");
        assert_eq!(snake_case("Vec3Normalize"), "vec3_normalize");
        assert_eq!(snake_case("UI"), "ui");
        assert_eq!(snake_case("game_world"), "game_world");
        assert_eq!(snake_case("gameWorld"), "game_world");
        assert_eq!(snake_case("7Days"), "_7_days");
        assert_eq!(snake_case("Self"), "self_");
    }

    #[test]
    fn module_names_drop_the_extension() {
        assert_eq!(module_name("game.exe"), "game");
        assert_eq!(module_name("GameAssembly.dll"), "game_assembly");
        assert_eq!(module_name("d3d11.dll"), "d3d11");
        assert_eq!(module_name("noext"), "noext");
        assert_eq!(module_name(".hidden"), "hidden");
    }

    #[test]
    fn plain_identifiers() {
        assert!(is_plain_identifier("game"));
        assert!(is_plain_identifier("_game2"));
        assert!(!is_plain_identifier("2game"));
        assert!(!is_plain_identifier("type"));
        assert!(!is_plain_identifier("self"));
        assert!(!is_plain_identifier("_"));
        assert!(!is_plain_identifier(""));
        assert!(!is_plain_identifier("game-1"));
    }

    #[test]
    fn idents_are_raw_only_for_keywords() {
        assert_eq!(ident("type").to_string(), "r#type");
        assert_eq!(ident("gen").to_string(), "r#gen");
        assert_eq!(ident("health").to_string(), "health");
    }

    #[test]
    fn scope_resolves_clashes() {
        let mut scope = Scope::default();
        scope.reserve("init");
        assert_eq!(scope.claim("init", |_| "init_9".to_owned()), "init_9");
        assert_eq!(scope.claim_numbered("x"), "x");
        assert_eq!(scope.claim_numbered("x"), "x_2");
        assert_eq!(scope.claim_numbered("x"), "x_3");
    }
}
