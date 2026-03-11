//! wasm-js-bridge CLI — build Rust crates into npm WASM packages.
//!
//! Compiles to WASM via cargo, then uses wasm-bindgen-cli-support directly to
//! emit a flat output directory:
//!
//! ```text
//! pkg/
//!   {stem}.js          ESM (bundler target)
//!   {stem}.cjs         CJS (nodejs target, truly CommonJS — not a .js rename trick)
//!   {stem}_bg.wasm     shared WASM binary (both JS files import this)
//!   {stem}.d.ts        TypeScript declarations (via ts-rs codegen test)
//!   {stem}.js.flow     Flow declarations (via flowjs-rs codegen test)
//! ```
//!
//! Source filename → output stem: `foo_bar.rs` → `fooBar`, `mod.rs` → parent dir name.

use std::path::{Path, PathBuf};
use std::process::Command;

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
        return Err(format!("cargo build --target wasm32-unknown-unknown failed with {status}"));
    }

    // cargo outputs to the workspace target dir; walk up to find it.
    let wasm_name = pkg_name.replace('-', "_");
    let wasm_path = crate_dir
        .join("target/wasm32-unknown-unknown/release")
        .join(format!("{wasm_name}.wasm"));

    // Also try workspace root target dir (two levels up from a workspace member).
    let wasm_path = if wasm_path.exists() {
        wasm_path
    } else {
        let workspace_target = crate_dir
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("target/wasm32-unknown-unknown/release"))
            .ok_or_else(|| format!("Cannot locate workspace target dir from {}", crate_dir.display()))?;
        let candidate = workspace_target.join(format!("{wasm_name}.wasm"));
        if candidate.exists() {
            candidate
        } else {
            return Err(format!(
                "Could not find {wasm_name}.wasm — tried:\n  {}\n  {}",
                crate_dir.join("target/wasm32-unknown-unknown/release").join(format!("{wasm_name}.wasm")).display(),
                candidate.display(),
            ));
        }
    };

    Ok(wasm_path)
}

/// Generate ESM `.js` + `_bg.wasm` into `out_dir` via wasm-bindgen bundler target.
fn generate_esm(wasm_path: &Path, out_dir: &Path, stem: &str) -> Result<(), String> {
    std::fs::create_dir_all(out_dir)
        .map_err(|e| format!("Failed to create {}: {e}", out_dir.display()))?;

    Bindgen::new()
        .input_path(wasm_path)
        .bundler(true)
        .map_err(|e| format!("wasm-bindgen bundler setup failed: {e}"))?
        .out_name(stem)
        .generate(out_dir)
        .map_err(|e| format!("wasm-bindgen ESM generation failed: {e}"))
}

/// Generate CJS `.cjs` into `out_dir` via wasm-bindgen nodejs target.
///
/// wasm-bindgen emits `{stem}.js` (nodejs); we rename it to `{stem}.cjs` so
/// the extension itself signals CommonJS — no `package.json` type trick needed.
/// The WASM import path inside the generated file uses `__dirname`-relative
/// `require()`, which stays correct as long as the `.cjs` and `_bg.wasm` are
/// co-located in the same directory.
fn generate_cjs(wasm_path: &Path, out_dir: &Path, stem: &str) -> Result<(), String> {
    std::fs::create_dir_all(out_dir)
        .map_err(|e| format!("Failed to create {}: {e}", out_dir.display()))?;

    Bindgen::new()
        .input_path(wasm_path)
        .nodejs(true)
        .map_err(|e| format!("wasm-bindgen nodejs setup failed: {e}"))?
        .out_name(stem)
        .generate(out_dir)
        .map_err(|e| format!("wasm-bindgen CJS generation failed: {e}"))?;

    // Rename {stem}.js → {stem}.cjs
    let js_path = out_dir.join(format!("{stem}.js"));
    let cjs_path = out_dir.join(format!("{stem}.cjs"));
    std::fs::rename(&js_path, &cjs_path).map_err(|e| {
        format!(
            "Failed to rename {} → {}: {e}",
            js_path.display(),
            cjs_path.display()
        )
    })
}

fn run_cargo_test_codegen(crate_dir: &Path, pkg_name: &str) -> Result<(), String> {
    let status = Command::new("cargo")
        .args([
            "test",
            "-p",
            pkg_name,
            "--features",
            "codegen",
            "--",
            "generate_npm_files",
        ])
        .current_dir(crate_dir)
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
    let out_dir = crate_dir.join("pkg");

    eprintln!("wasm-js-bridge: {pkg_name} → stem \"{stem}\"");

    // Step 1: Compile to WASM (needed for --js or --cjs)
    let wasm_path = if want_js || want_cjs {
        eprintln!("  → cargo build --target wasm32-unknown-unknown");
        let path = cargo_build_wasm(&crate_dir, &pkg_name, &meta.wasm_features)
            .unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(1);
            });
        Some(path)
    } else {
        None
    };

    // Step 2: ESM output
    if want_js {
        eprintln!("  → {stem}.js (ESM)");
        generate_esm(wasm_path.as_ref().unwrap(), &out_dir, &stem).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        });
    }

    // Step 3: CJS output (shares the WASM binary already in pkg/ from ESM step,
    // or generates its own if --cjs was requested without --js)
    if want_cjs {
        eprintln!("  → {stem}.cjs (CJS)");
        generate_cjs(wasm_path.as_ref().unwrap(), &out_dir, &stem).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        });
    }

    // Step 4: Type declarations (.d.ts + .js.flow via cargo test codegen)
    if want_dts || want_flow {
        eprintln!("  → {stem}.d.ts + {stem}.js.flow (codegen)");
        run_cargo_test_codegen(&crate_dir, &pkg_name).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        });
    }

    eprintln!("wasm-js-bridge: done");
}

#[cfg(test)]
mod tests {
    use super::*;

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
