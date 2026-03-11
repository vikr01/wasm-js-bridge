//! wasm-js-bridge CLI — build Rust crates into npm WASM packages.
//!
//! Orchestrates wasm-pack (ESM + CJS) and cargo test (type generation)
//! in a single command. Output filenames are derived from source filenames:
//! `foo_bar.rs` → `fooBar.js` + `fooBar.cjs` + `fooBar.d.ts` + `fooBar.js.flow`.

use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser;

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
    /// Output ESM `.js` via wasm-pack --target bundler.
    #[arg(long)]
    js: bool,

    /// Output CJS via wasm-pack --target nodejs.
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

    /// Extra cargo flags passed after `--` to wasm-pack (e.g. `"--features wasm"`).
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
/// `"src/foo_bar.rs"` → `"fooBar"`, `"src/lib.rs"` → `"lib"`, `"src/wasm.rs"` → `"wasm"`.
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
            format!("Invalid [package.metadata.wasm-js-bridge] in {}: {e}", cargo_toml_path.display())
        })?,
        None => WjbMeta::default(),
    };

    Ok((pkg_name, meta))
}

// ---------------------------------------------------------------------------
// Build steps
// ---------------------------------------------------------------------------

fn ensure_tool(name: &str) -> Result<(), String> {
    Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|_| format!("{name} not found. Install it: https://rustwasm.github.io/wasm-pack/installer/"))?;
    Ok(())
}

fn run_wasm_pack(
    crate_dir: &Path,
    target: &str,
    out_dir: &str,
    out_name: &str,
    cargo_flags: &str,
) -> Result<(), String> {
    let mut args = vec![
        "build".to_string(),
        ".".to_string(),
        "--target".to_string(),
        target.to_string(),
        "--out-dir".to_string(),
        out_dir.to_string(),
        "--out-name".to_string(),
        out_name.to_string(),
    ];

    if !cargo_flags.is_empty() {
        args.push("--".to_string());
        args.extend(cargo_flags.split_whitespace().map(String::from));
    }

    let status = Command::new("wasm-pack")
        .args(&args)
        .current_dir(crate_dir)
        .status()
        .map_err(|e| format!("Failed to run wasm-pack: {e}"))?;

    if !status.success() {
        return Err(format!("wasm-pack {target} failed with {status}"));
    }
    Ok(())
}

fn run_cargo_test_codegen(
    crate_dir: &Path,
    pkg_name: &str,
    codegen_feature: &str,
) -> Result<(), String> {
    let status = Command::new("cargo")
        .args([
            "test",
            "-p",
            pkg_name,
            "--features",
            codegen_feature,
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

fn write_cjs_package_json(crate_dir: &Path) -> Result<(), String> {
    let cjs_dir = crate_dir.join("pkg/cjs");
    std::fs::create_dir_all(&cjs_dir)
        .map_err(|e| format!("Failed to create {}: {e}", cjs_dir.display()))?;
    std::fs::write(cjs_dir.join("package.json"), "{\"type\":\"commonjs\"}\n")
        .map_err(|e| format!("Failed to write pkg/cjs/package.json: {e}"))?;
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

    eprintln!("wasm-js-bridge: {pkg_name} → stem \"{stem}\"");

    if want_js || want_cjs {
        ensure_tool("wasm-pack").unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        });
    }

    // Step 1: ESM output (wasm-pack --target bundler)
    if want_js {
        eprintln!("  → {stem}.js (ESM)");
        run_wasm_pack(&crate_dir, "bundler", "pkg", &stem, &meta.wasm_features).unwrap_or_else(
            |e| {
                eprintln!("{e}");
                std::process::exit(1);
            },
        );
    }

    // Step 2: CJS output (wasm-pack --target nodejs)
    if want_cjs {
        eprintln!("  → {stem}.js (CJS)");
        run_wasm_pack(
            &crate_dir,
            "nodejs",
            "pkg/cjs",
            &stem,
            &meta.wasm_features,
        )
        .unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        });
        write_cjs_package_json(&crate_dir).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        });
    }

    // Step 3: Type declarations (.d.ts + .js.flow via cargo test codegen)
    if want_dts || want_flow {
        eprintln!("  → {stem}.d.ts + {stem}.js.flow (codegen)");
        run_cargo_test_codegen(&crate_dir, &pkg_name, "codegen").unwrap_or_else(|e| {
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
