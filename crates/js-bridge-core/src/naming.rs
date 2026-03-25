use std::path::Path;

pub fn snake_to_camel(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = false;
    for c in s.chars() {
        if c == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }
    result
}

/// Derive the camelCase output stem from a source file path.
///
/// `"src/foo_bar.rs"` → `"fooBar"`, `"src/lib.rs"` → `"lib"`.
/// For `mod.rs`, the parent directory name is used.
pub fn file_to_stem(file: &str) -> String {
    let path = Path::new(file);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    let base = if stem == "mod" {
        let parent = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("mod");
        if parent == "src" {
            "mod"
        } else {
            parent
        }
    } else {
        stem
    };
    snake_to_camel(base)
}

pub fn is_valid_js_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let valid_start = first == '_' || first == '$' || first.is_ascii_alphabetic();
    valid_start && chars.all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric())
}
