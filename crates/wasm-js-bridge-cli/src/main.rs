//! wasm-js-bridge CLI — build Rust crates into npm WASM packages.
//!
//! Compiles to WASM via cargo, then uses wasm-bindgen-cli-support in-memory
//! API to emit fully self-contained JS files with the WASM binary inlined:
//!
//! ```text
//! {out_dir}/
//!   {stem}.js      ESM — WASM inlined as base64, no external .wasm file
//!   {stem}.cjs     CJS — WASM inlined as base64, no external .wasm file
//!   {stem}.d.ts    TypeScript declarations (via ts-rs codegen test)
//!   {stem}.js.flow Flow declarations (via flowjs-rs codegen test)
//! ```
//!
//! Source filename → output stem: `foo_bar.rs` → `fooBar`, `mod.rs` → parent dir name.

mod inline;

use std::path::{Path, PathBuf};
use std::process::Command;

const EXT_ESM: &str = "js";
const EXT_CJS: &str = "cjs";
const EXT_DTS: &str = "d.ts";
const EXT_FLOW: &str = "js.flow";

use clap::Parser;
use wasm_bindgen_cli_support::Bindgen;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "wasm-js-bridge", about = "Package Rust crates as npm WASM packages")]
enum Cli {
    /// Build the crate into an npm package.
    Build(BuildArgs),
}

#[derive(Parser)]
struct BuildArgs {
    /// Output ESM `.js`.
    #[arg(long)]
    js: bool,

    /// Output CJS `.cjs`.
    #[arg(long)]
    cjs: bool,

    /// Output TypeScript declarations (`.d.ts`) via ts-rs.
    #[arg(long)]
    dts: bool,

    /// Output Flow declarations (`.js.flow`) via flowjs-rs.
    #[arg(long)]
    flow: bool,

    /// Shorthand for --js --cjs --dts --flow.
    #[arg(long)]
    all: bool,

    /// Path to the crate directory (default: current directory).
    #[arg(long, default_value = ".")]
    path: PathBuf,

    /// Output directory (default: `<crate>/pkg`).
    #[arg(long)]
    out_dir: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Config (from Cargo.toml metadata)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, Default)]
struct WjbMeta {
    /// Source file that contains the `bundle!` invocation (e.g. `"src/lib.rs"`).
    /// Used to derive the output stem. Default: `"src/lib.rs"`.
    #[serde(default = "default_entry")]
    entry: String,

    /// Cargo features to pass when building for WASM (e.g. `"--features wasm"`).
    #[serde(default = "default_wasm_features")]
    wasm_features: String,
}

fn default_entry() -> String {
    "src/lib.rs".to_string()
}

fn default_wasm_features() -> String {
    "--features wasm".to_string()
}

// ---------------------------------------------------------------------------
// Naming
// ---------------------------------------------------------------------------

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

/// Derive the camelCase output stem from a source file path.
///
/// `"src/foo_bar.rs"` → `"fooBar"`, `"src/lib.rs"` → `"lib"`.
/// For `mod.rs`, the parent directory name is used.
fn file_to_stem(file: &str) -> String {
    let path = Path::new(file);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    let base = if stem == "mod" {
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("mod")
    } else {
        stem
    };
    snake_to_camel(base)
}

// ---------------------------------------------------------------------------
// Cargo.toml parsing
// ---------------------------------------------------------------------------

fn read_meta(crate_dir: &Path) -> Result<(String, WjbMeta), String> {
    let cargo_toml_path = crate_dir.join("Cargo.toml");
    let raw = std::fs::read_to_string(&cargo_toml_path)
        .map_err(|e| format!("Failed to read {}: {e}", cargo_toml_path.display()))?;
    let doc: toml::Value = raw
        .parse()
        .map_err(|e| format!("Failed to parse {}: {e}", cargo_toml_path.display()))?;

    let pkg_name = doc
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .ok_or("Missing [package] name in Cargo.toml")?
        .to_string();

    let meta: WjbMeta = match doc
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("wasm-js-bridge"))
    {
        Some(v) => v.clone().try_into().map_err(|e| {
            format!(
                "Invalid [package.metadata.wasm-js-bridge] in {}: {e}",
                cargo_toml_path.display()
            )
        })?,
        None => WjbMeta::default(),
    };

    Ok((pkg_name, meta))
}

// ---------------------------------------------------------------------------
// Build steps
// ---------------------------------------------------------------------------

/// Run `cargo build --target wasm32-unknown-unknown` and return the path to
/// the produced `.wasm` file.
fn cargo_build_wasm(
    crate_dir: &Path,
    pkg_name: &str,
    cargo_flags: &str,
) -> Result<PathBuf, String> {
    let mut args = vec![
        "build".to_string(),
        "--target".to_string(),
        "wasm32-unknown-unknown".to_string(),
        "--release".to_string(),
    ];

    if !cargo_flags.is_empty() {
        args.extend(cargo_flags.split_whitespace().map(String::from));
    }

    let status = Command::new("cargo")
        .args(&args)
        .current_dir(crate_dir)
        .status()
        .map_err(|e| format!("Failed to run cargo build: {e}"))?;

    if !status.success() {
        return Err(format!(
            "cargo build --target wasm32-unknown-unknown failed with {status}"
        ));
    }

    // cargo outputs to the workspace target dir; check local then walk up.
    let wasm_name = pkg_name.replace('-', "_");
    let local = crate_dir
        .join("target/wasm32-unknown-unknown/release")
        .join(format!("{wasm_name}.wasm"));
    if local.exists() {
        return Ok(local);
    }

    let workspace = crate_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| {
            p.join("target/wasm32-unknown-unknown/release")
                .join(format!("{wasm_name}.wasm"))
        })
        .ok_or_else(|| {
            format!(
                "Cannot locate {wasm_name}.wasm — tried {}",
                local.display()
            )
        })?;

    if workspace.exists() {
        Ok(workspace)
    } else {
        Err(format!(
            "Cannot locate {wasm_name}.wasm — tried:\n  {}\n  {}",
            local.display(),
            workspace.display(),
        ))
    }
}

/// Generate a self-contained CJS file with the WASM binary inlined as base64.
///
/// Uses wasm-bindgen nodejs target in-memory, then patches the `readFileSync`
/// WASM load with an inline `Buffer.from('BASE64', 'base64')` before writing.
/// No `_bg.wasm` file is emitted.
fn generate_cjs(wasm_path: &Path, out_dir: &Path, stem: &str) -> Result<(), String> {
    std::fs::create_dir_all(out_dir)
        .map_err(|e| format!("Failed to create {}: {e}", out_dir.display()))?;

    let mut output = Bindgen::new()
        .input_path(wasm_path)
        .nodejs(true)
        .map_err(|e| format!("wasm-bindgen nodejs setup failed: {e}"))?
        .out_name(stem)
        .generate_output()
        .map_err(|e| format!("wasm-bindgen CJS generation failed: {e}"))?;

    let wasm_bytes = output.wasm_mut().emit_wasm();
    let js = inline::inline_wasm_cjs(output.js(), &wasm_bytes)?;

    std::fs::write(out_dir.join(format!("{stem}.{EXT_CJS}")), js)
        .map_err(|e| format!("Failed to write {stem}.{EXT_CJS}: {e}"))
}

/// Generate a self-contained ESM file with the WASM binary inlined as base64.
///
/// Uses wasm-bindgen nodejs target in-memory (synchronous init, easier to
/// inline than the async bundler target), patches the WASM load with an inline
/// `Uint8Array.from(atob('BASE64'), ...)`, then rewraps as ESM. No `_bg.wasm`
/// file is emitted.
fn generate_esm(wasm_path: &Path, out_dir: &Path, stem: &str) -> Result<(), String> {
    std::fs::create_dir_all(out_dir)
        .map_err(|e| format!("Failed to create {}: {e}", out_dir.display()))?;

    let mut output = Bindgen::new()
        .input_path(wasm_path)
        .nodejs(true)
        .map_err(|e| format!("wasm-bindgen nodejs setup failed: {e}"))?
        .out_name(stem)
        .generate_output()
        .map_err(|e| format!("wasm-bindgen ESM generation failed: {e}"))?;

    let wasm_bytes = output.wasm_mut().emit_wasm();
    let js = inline::inline_wasm_esm(output.js(), &wasm_bytes)?;

    // wasm-bindgen nodejs output uses require/module.exports; convert to ESM.
    let esm = cjs_to_esm(&js, stem);

    std::fs::write(out_dir.join(format!("{stem}.{EXT_ESM}")), esm)
        .map_err(|e| format!("Failed to write {stem}.{EXT_ESM}: {e}"))
}

/// Wrap a patched CJS string as an ESM module.
///
/// wasm-bindgen nodejs target uses `exports.name = value` for each public
/// export and `const { ... } = require(...)` for imports. This function
/// converts both to their ESM equivalents. The `require('fs')` call has
/// already been removed by the inlining step.
///
/// Handles:
/// - `const { X, Y } = require('module')` → `import { X, Y } from 'module'`
/// - `const { X: Y } = require('module')` → `import { X as Y } from 'module'`
/// - `const mod = require('module')` → `import * as mod from 'module'`
/// - `exports.name = value` → collected and emitted as `export { name }` stubs
///   with the value assigned to a local binding first.
fn cjs_to_esm(cjs: &str, _stem: &str) -> String {
    let mut esm_imports: Vec<String> = Vec::new();
    let mut export_names: Vec<String> = Vec::new();
    let mut out = String::with_capacity(cjs.len());

    for line in cjs.lines() {
        let trimmed = line.trim_start();

        // const { X, Y } = require('mod') or const { X: Z } = require('mod')
        if trimmed.starts_with("const {") && trimmed.contains("} = require(") {
            if let Some(import_stmt) = destructured_require_to_import(trimmed) {
                esm_imports.push(import_stmt);
                continue;
            }
        }

        // const mod = require('mod') — namespace import
        if trimmed.starts_with("const ") && trimmed.contains(" = require(") && !trimmed.contains('{') {
            if let Some(import_stmt) = namespace_require_to_import(trimmed) {
                esm_imports.push(import_stmt);
                continue;
            }
        }

        // exports.name = value  → collect export name, keep the assignment
        if let Some(after_exports) = trimmed.strip_prefix("exports.") {
            if let Some(name) = extract_exports_name(trimmed) {
                // Rewrite as a local `let name = value` and record for re-export
                let rest = after_exports.trim_start();
                // rest = "name = value..."
                if let Some(eq) = rest.find(" = ") {
                    let value_part = &rest[eq + 3..];
                    out.push_str("let ");
                    out.push_str(&name);
                    out.push_str(" = ");
                    out.push_str(value_part);
                    out.push('\n');
                    export_names.push(name);
                    continue;
                }
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    // Prepend ESM imports
    let mut result = String::new();
    for imp in &esm_imports {
        result.push_str(imp);
        result.push('\n');
    }
    result.push_str(&out);

    // Append named exports
    if !export_names.is_empty() {
        result.push_str("export { ");
        result.push_str(&export_names.join(", "));
        result.push_str(" };\n");
    }

    result
}

/// Convert `const { X, Y: Z } = require('mod')` to `import { X, Y as Z } from 'mod'`.
fn destructured_require_to_import(line: &str) -> Option<String> {
    let after_const = line.strip_prefix("const {")?;
    let brace_end = after_const.find("} = require(")?;
    let bindings_str = &after_const[..brace_end];
    let after_eq = &after_const[brace_end + "} = require(".len()..];
    // after_eq = "'mod');" or "`mod`);"
    let module = extract_require_module(after_eq)?;

    let bindings: Vec<String> = bindings_str
        .split(',')
        .map(|b| {
            let b = b.trim();
            if let Some((orig, alias)) = b.split_once(':') {
                format!("{} as {}", orig.trim(), alias.trim())
            } else {
                b.to_string()
            }
        })
        .filter(|b| !b.is_empty())
        .collect();

    Some(format!("import {{ {} }} from '{}';", bindings.join(", "), module))
}

/// Convert `const mod = require('mod')` to `import * as mod from 'mod'`.
fn namespace_require_to_import(line: &str) -> Option<String> {
    let after_const = line.strip_prefix("const ")?;
    let eq = after_const.find(" = require(")?;
    let name = after_const[..eq].trim();
    let after_eq = &after_const[eq + " = require(".len()..];
    let module = extract_require_module(after_eq)?;
    Some(format!("import * as {name} from '{module}';"))
}

/// Extract the module name from the tail of a `require(...)` call.
///
/// Accepts `'mod')`, `"mod")`, `` `mod`) ``.
fn extract_require_module(s: &str) -> Option<String> {
    let s = s.trim_start();
    let (quote, rest) = if let Some(r) = s.strip_prefix('\'') {
        ('\'', r)
    } else if let Some(r) = s.strip_prefix('"') {
        ('"', r)
    } else if let Some(r) = s.strip_prefix('`') {
        ('`', r)
    } else {
        return None;
    };
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

/// Extract the export name from an `exports.name = ...` line.
fn extract_exports_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix("exports.")?;
    let eq = rest.find(" = ").or_else(|| rest.find('='))?;
    let name = rest[..eq].trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

fn run_cargo_test_codegen(crate_dir: &Path, pkg_name: &str, out_dir: &Path) -> Result<(), String> {
    let status = Command::new("cargo")
        .args([
            "test",
            "-p",
            pkg_name,
            "--features",
            "codegen,ts,flow",
            "--",
            "generate_npm_files",
        ])
        .current_dir(crate_dir)
        // Pass out_dir so bundle! macro writes .d.ts/.js.flow to the right place.
        .env("WJB_OUT_DIR", out_dir)
        .status()
        .map_err(|e| format!("Failed to run cargo test: {e}"))?;

    if !status.success() {
        return Err(format!("cargo test codegen failed with {status}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let Cli::Build(args) = Cli::parse();

    let want_js = args.js || args.all;
    let want_cjs = args.cjs || args.all;
    let want_dts = args.dts || args.all;
    let want_flow = args.flow || args.all;

    if !want_js && !want_cjs && !want_dts && !want_flow {
        eprintln!("Nothing to build. Use --all or specify outputs: --js --cjs --dts --flow");
        std::process::exit(1);
    }

    let crate_dir = std::fs::canonicalize(&args.path).unwrap_or_else(|e| {
        eprintln!("Invalid path {}: {e}", args.path.display());
        std::process::exit(1);
    });

    let (pkg_name, meta) = read_meta(&crate_dir).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });

    let stem = file_to_stem(&meta.entry);
    let out_dir = args
        .out_dir
        .unwrap_or_else(|| crate_dir.join("pkg"));

    eprintln!("wasm-js-bridge: {pkg_name} → stem \"{stem}\"");

    let step = |outputs: &str| eprintln!("  → {outputs}");
    let run = |result: Result<(), String>| {
        result.unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        })
    };
    let run_t = |result: Result<PathBuf, String>| {
        result.unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        })
    };

    // Step 1: Compile to WASM (needed for --js or --cjs)
    let wasm_path = if want_js || want_cjs {
        step("cargo build --target wasm32-unknown-unknown");
        Some(run_t(cargo_build_wasm(&crate_dir, &pkg_name, &meta.wasm_features)))
    } else {
        None
    };

    if want_js {
        step(&format!("{stem}.{EXT_ESM}"));
        run(generate_esm(wasm_path.as_ref().unwrap(), &out_dir, &stem));
    }

    if want_cjs {
        step(&format!("{stem}.{EXT_CJS}"));
        run(generate_cjs(wasm_path.as_ref().unwrap(), &out_dir, &stem));
    }

    if want_dts || want_flow {
        step(&format!("{stem}.{EXT_DTS} + {stem}.{EXT_FLOW}"));
        run(run_cargo_test_codegen(&crate_dir, &pkg_name, &out_dir));
    }

    eprintln!("wasm-js-bridge: done");
}

#[cfg(test)]
mod tests {
    use super::*;

    // Actual wasm-bindgen 0.2.114 non-threaded nodejs CJS output (trimmed).
    const WBG_CJS_NON_THREADED: &str = r#"
'use strict';

const { TextDecoder, TextEncoder } = require(`util`);

let wasm;

exports.parseSelector = parseSelector;

const wasmPath = `${__dirname}/my_crate_bg.wasm`;
const wasmBytes = require('fs').readFileSync(wasmPath);
const wasmModule = new WebAssembly.Module(wasmBytes);
let instance = new WebAssembly.Instance(wasmModule, __wbg_get_imports()).exports;
wasm = instance;
"#;

    #[test]
    fn cjs_to_esm_destructured_require() {
        // Arrange and Act
        let result = cjs_to_esm("const { TextDecoder, TextEncoder } = require(`util`);\n", "test");

        // Assert
        assert!(result.contains("import { TextDecoder, TextEncoder } from 'util'"), "should convert destructured require: {result}");
        assert!(!result.contains("require("), "no require should remain: {result}");
    }

    #[test]
    fn cjs_to_esm_exports_dot() {
        // Arrange and Act
        let result = cjs_to_esm("exports.parseSelector = parseSelector;\n", "test");

        // Assert
        assert!(result.contains("export {"), "should emit named export: {result}");
        assert!(result.contains("parseSelector"), "export should include name: {result}");
        assert!(!result.contains("exports."), "exports. should be rewritten: {result}");
    }

    #[test]
    fn cjs_to_esm_non_threaded_full() {
        // Arrange and Act
        let result = cjs_to_esm(WBG_CJS_NON_THREADED, "myCrate");

        // Assert
        assert!(result.contains("import { TextDecoder, TextEncoder }"), "util import converted");
        assert!(!result.contains("require(`util`)"), "util require gone");
        assert!(result.contains("export {"), "exports.* converted to named export");
    }

    #[test]
    fn file_to_stem_basic() {
        // Arrange, Act, and Assert
        assert_eq!(file_to_stem("src/lib.rs"), "lib", "lib.rs → lib");
        assert_eq!(file_to_stem("src/wasm.rs"), "wasm", "wasm.rs → wasm");
        assert_eq!(
            file_to_stem("src/foo_bar.rs"),
            "fooBar",
            "snake_case → camelCase"
        );
        assert_eq!(
            file_to_stem("src/index_bar.rs"),
            "indexBar",
            "index_bar.rs → indexBar"
        );
    }

    #[test]
    fn file_to_stem_mod_rs() {
        // Arrange, Act, and Assert
        assert_eq!(
            file_to_stem("src/navigate/mod.rs"),
            "navigate",
            "mod.rs → parent dir"
        );
    }
}
