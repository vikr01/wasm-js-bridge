//! Build tool for packaging Rust crates as npm WASM packages with TypeScript and Flow types.
//!
//! Produces `.js`, `.d.ts`, and `.js.flow` output files from Rust-defined descriptors,
//! one set per source file stem. Feature-gated: `ts` for TypeScript/JavaScript, `flow` for Flow.

mod types;

mod flow;
mod ts;

pub use flow::{generate_index_flow, OpaqueType};
pub use ts::generate_index_dts;
pub use types::{Interface, TypeAlias, WasmFn};
pub use wasm_js_bridge_macros::{bundle, wasm_export, wasm_peers};

/// Convert a `file!()` path to a camelCase output file stem.
///
/// The stem is derived purely from the source filename — no special cases.
/// `"src/foo_bar.rs"` → `"fooBar"`, `"src/wasm.rs"` → `"wasm"`, `"src/lib.rs"` → `"lib"`.
/// For `mod.rs`, the parent directory name is used instead.
pub fn file_to_stem(file: &str) -> String {
    use std::path::Path;
    let path = Path::new(file);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    let base = if stem == "mod" {
        // mod.rs → use parent directory name
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

fn snake_to_camel(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_to_stem_lib() {
        assert_eq!(file_to_stem("src/lib.rs"), "lib", "lib.rs → lib");
    }

    #[test]
    fn file_to_stem_index() {
        assert_eq!(file_to_stem("src/index.rs"), "index", "index.rs → index");
    }

    #[test]
    fn file_to_stem_snake_case() {
        assert_eq!(
            file_to_stem("src/foo_bar.rs"),
            "fooBar",
            "snake_case → camelCase"
        );
    }

    #[test]
    fn file_to_stem_mod_rs() {
        assert_eq!(
            file_to_stem("src/query/mod.rs"),
            "query",
            "mod.rs → parent dir"
        );
    }

    #[test]
    fn file_to_stem_root_mod_rs() {
        assert_eq!(
            file_to_stem("src/mod.rs"),
            "mod",
            "root mod.rs should not become src"
        );
    }

    #[test]
    fn file_to_stem_deep_path() {
        assert_eq!(
            file_to_stem("crates/wasm-js-bridge/src/query_options.rs"),
            "queryOptions",
            "deep path strips dirs and converts"
        );
    }
}
