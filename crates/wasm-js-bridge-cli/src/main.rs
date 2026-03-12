//! wasm-js-bridge CLI — build Rust crates into npm WASM packages.
//!
//! Compiles to WASM via cargo, invokes the `wasm-bindgen` CLI from PATH to
//! produce JS bindings, then inlines the WASM binary as base64 so no external
//! `.wasm` file is needed at runtime.
//!
//! ```text
//! {out_dir}/
//!   {stem}.js      ESM — WASM inlined as base64, no external .wasm file
//!   {stem}.cjs     CJS — WASM inlined as base64, no external .wasm file
//!   {stem}.d.ts    TypeScript declarations (via ts-rs codegen test)
//!   {stem}.js.flow Flow declarations (via flowjs-rs codegen test)
//! ```
//!
//! The `wasm-bindgen` CLI version must match the `wasm-bindgen` crate version
//! used by the target crate. Install the matching version with:
//!
//! ```sh
//! cargo install wasm-bindgen-cli --version <version>
//! ```
//!
//! Source filename → output stem: `foo_bar.rs` → `fooBar`, `mod.rs` → parent dir name.

mod inline;
mod logger;
mod peer;

use std::path::{Path, PathBuf};
use std::process::Command;

const EXT_ESM: &str = "js";
const EXT_CJS: &str = "cjs";
const EXT_DTS: &str = "d.ts";
const EXT_FLOW: &str = "js.flow";

use clap::Parser;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "wasm-js-bridge",
    about = "Package Rust crates as npm WASM packages"
)]
enum Cli {
    /// Build a single crate into an npm package.
    Build(BuildArgs),
    /// Build all npm-packaged crates in the workspace in dependency order,
    /// wiring WASM peer imports between packages automatically.
    BuildWorkspace(BuildWorkspaceArgs),
}

#[derive(Parser)]
struct BuildWorkspaceArgs {
    /// Output directory root. Each package is written to `<out_dir>/<pkg_name>/`.
    #[arg(long)]
    out_dir: Option<PathBuf>,
}

#[derive(Parser)]
struct BuildArgs {
    /// Override: produce ESM `.js` (always on by default; this flag is a no-op unless
    /// combined with other flags to be explicit).
    #[arg(long)]
    js: bool,

    /// Override: produce CJS `.cjs` (default from metadata; flag forces it on).
    #[arg(long)]
    cjs: bool,

    /// Override: produce TypeScript `.d.ts` (default from metadata; flag forces it on).
    #[arg(long)]
    dts: bool,

    /// Override: produce Flow `.js.flow` (default from metadata; flag forces it on).
    #[arg(long)]
    flow: bool,

    /// Force all four outputs regardless of metadata config.
    #[arg(long)]
    all: bool,

    /// Path to the crate directory (default: current directory).
    #[arg(long, default_value = ".")]
    path: PathBuf,

    /// Output directory (default: crate root).
    #[arg(long)]
    out_dir: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Config (from Cargo.toml metadata)
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

/// `[package.metadata.wasm-js-bridge]` configuration.
///
/// Output flags apply to every export in the package — you cannot mix formats
/// per source file. ESM (`.js`) and TypeScript (`.d.ts`) are on by default.
///
/// ```toml
/// [package.metadata.wasm-js-bridge]
/// npm_name      = "@aql/predicates"
/// wasm_features = "--features wasm"
/// cjs    = true   # opt in to CJS shim
/// jsflow = true   # opt in to Flow declarations
/// # dts = true    # default; set false to suppress
///
/// [package.metadata.wasm-js-bridge.exports]
/// "."      = "src/foo_bar.rs"   # stem derived: fooBar
/// "./util" = "src/util.rs"      # stem derived: util
/// ```
#[derive(serde::Deserialize)]
struct WjbMeta {
    /// Cargo features to pass when building for WASM (e.g. `"--features wasm"`).
    #[serde(default = "default_wasm_features")]
    wasm_features: String,

    /// npm package name (e.g. `"@aql/predicates"`). Required for package.json generation.
    npm_name: Option<String>,

    /// Produce a CJS `.cjs` shim. Default: `false`.
    #[serde(default)]
    cjs: bool,

    /// Produce TypeScript `.d.ts` declarations. Default: `true`.
    #[serde(default = "default_true")]
    dts: bool,

    /// Produce Flow `.js.flow` declarations. Default: `false`.
    #[serde(default)]
    jsflow: bool,

    /// package.json subpath → source file path.
    ///
    /// The output stem is derived from the source filename:
    /// `"src/foo_bar.rs"` → stem `"fooBar"`.
    /// Default when absent: `{ ".": "src/lib.rs" }`.
    #[serde(default)]
    exports: std::collections::BTreeMap<String, String>,
}

impl Default for WjbMeta {
    fn default() -> Self {
        Self {
            wasm_features: default_wasm_features(),
            npm_name: None,
            cjs: false,
            dts: true,
            jsflow: false,
            exports: std::collections::BTreeMap::new(),
        }
    }
}

fn default_wasm_features() -> String {
    "--features wasm".to_string()
}

/// Return the exports map, defaulting to `{ ".": "src/lib.rs" }`.
fn resolved_exports(meta: &WjbMeta) -> std::collections::BTreeMap<String, String> {
    if meta.exports.is_empty() {
        std::iter::once((".".to_string(), "src/lib.rs".to_string())).collect()
    } else {
        meta.exports.clone()
    }
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
    peer_shim: Option<&Path>,
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

    let mut cmd = Command::new("cargo");
    cmd.args(&args).current_dir(crate_dir);
    if let Some(shim) = peer_shim {
        cmd.env("WJB_PEER_SHIM", shim);
    }

    let status = cmd
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
        .ok_or_else(|| format!("Cannot locate {wasm_name}.wasm — tried {}", local.display()))?;

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

/// Walk up from `crate_dir` to find the workspace `Cargo.lock`.
fn find_cargo_lock(crate_dir: &Path) -> Option<PathBuf> {
    let mut dir = crate_dir;
    loop {
        let candidate = dir.join("Cargo.lock");
        if candidate.exists() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
}

/// Read the `wasm-bindgen` version from `Cargo.lock`.
///
/// Returns `None` if the lockfile cannot be found or parsed, or if the crate
/// is not present (e.g. the crate doesn't depend on wasm-bindgen).
fn find_wasm_bindgen_version(crate_dir: &Path) -> Option<String> {
    let lock_path = find_cargo_lock(crate_dir)?;
    let content = std::fs::read_to_string(lock_path).ok()?;
    let doc: toml::Value = content.parse().ok()?;
    doc.get("package")?
        .as_array()?
        .iter()
        .find(|p| p["name"].as_str() == Some("wasm-bindgen"))
        .and_then(|p| p["version"].as_str())
        .map(String::from)
}

/// Validate that the `wasm-bindgen` CLI on PATH matches `expected_version`.
///
/// Emits a warning (not a hard error) if the version cannot be determined or
/// does not match, since minor patch differences are sometimes compatible.
fn check_wasm_bindgen_version(expected: &str, log: &logger::Logger) {
    let output = match Command::new("wasm-bindgen").arg("--version").output() {
        Ok(o) => o,
        Err(e) => {
            log.warn(&format!(
                "Could not run wasm-bindgen: {e}. Install: cargo install wasm-bindgen-cli --version {expected}"
            ));
            return;
        }
    };
    // Output format: "wasm-bindgen 0.2.108\n"
    let stdout = String::from_utf8_lossy(&output.stdout);
    let installed = stdout
        .trim()
        .strip_prefix("wasm-bindgen ")
        .unwrap_or("")
        .trim();
    if installed != expected {
        log.warn(&format!(
            "wasm-bindgen version mismatch: installed {installed}, Cargo.lock requires {expected}.\n  Fix: cargo install wasm-bindgen-cli --version {expected}"
        ));
    }
}

/// Invoke the `wasm-bindgen` CLI (from PATH) on `wasm_path` and return the
/// raw nodejs JS string and WASM bytes.
///
/// Uses a temporary directory for the intermediate files, which is cleaned up
/// before returning regardless of success or failure. The caller is responsible
/// for further processing (inlining, ESM conversion, etc.).
fn run_wasm_bindgen(wasm_path: &Path, stem: &str) -> Result<(String, Vec<u8>), String> {
    let tmp = std::env::temp_dir().join(format!("wjb-{}-{stem}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("Failed to create temp dir: {e}"))?;

    let result = (|| {
        let status = Command::new("wasm-bindgen")
            .args(["--target", "nodejs", "--out-name", stem, "--out-dir"])
            .arg(&tmp)
            .arg(wasm_path)
            .status()
            .map_err(|e| format!("Failed to run wasm-bindgen: {e}."))?;

        if !status.success() {
            return Err(format!("wasm-bindgen failed with {status}."));
        }

        let js = std::fs::read_to_string(tmp.join(format!("{stem}.js")))
            .map_err(|e| format!("Failed to read wasm-bindgen JS output: {e}"))?;
        let wasm_bytes = std::fs::read(tmp.join(format!("{stem}_bg.wasm")))
            .map_err(|e| format!("Failed to read wasm-bindgen WASM output: {e}"))?;

        Ok((js, wasm_bytes))
    })();

    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// Write a self-contained CJS file with the WASM binary inlined as base64.
fn write_cjs(js: &str, wasm_bytes: &[u8], out_dir: &Path, stem: &str) -> Result<(), String> {
    let patched = inline::inline_wasm_cjs(js, wasm_bytes)?;
    std::fs::write(out_dir.join(format!("{stem}.{EXT_CJS}")), patched)
        .map_err(|e| format!("Failed to write {stem}.{EXT_CJS}: {e}"))
}

/// Write a self-contained ESM file with the WASM binary inlined as base64.
fn write_esm(js: &str, wasm_bytes: &[u8], out_dir: &Path, stem: &str) -> Result<(), String> {
    let patched = inline::inline_wasm_esm(js, wasm_bytes)?;
    let esm = cjs_to_esm(&patched, stem);
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
    let mut seen_imports: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut export_names: Vec<String> = Vec::new();
    let mut seen_exports: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = String::with_capacity(cjs.len());

    for line in cjs.lines() {
        let trimmed = line.trim_start();

        if let Some(import_stmt) = require_to_import(trimmed) {
            if seen_imports.insert(import_stmt.clone()) {
                esm_imports.push(import_stmt);
            }
            continue;
        }

        // exports.name = value  → collect export name, emit binding only when needed.
        if let Some((name, value_part)) = parse_exports_assignment(trimmed) {
            if !is_identity_export(&name, &value_part) {
                out.push_str("const ");
                out.push_str(&name);
                out.push_str(" = ");
                out.push_str(&value_part);
                out.push_str(";\n");
            }
            if seen_exports.insert(name.clone()) {
                export_names.push(name);
            }
            continue;
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

/// Convert `const ... = require('mod')` forms to ESM imports.
///
/// Supported:
/// - `const { X, Y: Z } = require('mod')`
/// - `const mod = require('mod')`
fn require_to_import(line: &str) -> Option<String> {
    let after_const = line.strip_prefix("const ")?.trim();
    let (lhs, rhs) = after_const.split_once('=')?;
    let lhs = lhs.trim();
    let module = parse_require_call(rhs.trim())?;

    if lhs.starts_with('{') && lhs.ends_with('}') {
        let inner = &lhs[1..lhs.len() - 1];
        let bindings: Vec<String> = inner
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
        if bindings.is_empty() {
            return None;
        }
        return Some(format!(
            "import {{ {} }} from '{}';",
            bindings.join(", "),
            module
        ));
    }

    if is_valid_js_identifier(lhs) {
        return Some(format!("import * as {lhs} from '{module}';"));
    }

    None
}

/// Parse `require("module")` and return the module name.
///
/// Requires the entire expression to be exactly a `require(...)` call (ignoring
/// whitespace and an optional trailing semicolon).
fn parse_require_call(s: &str) -> Option<String> {
    let s = s.trim().trim_end_matches(';').trim();
    let rest = s.strip_prefix("require")?.trim_start();
    let inner = rest.strip_prefix('(')?;
    let close = inner.rfind(')')?;
    if !inner[close + 1..].trim().is_empty() {
        return None;
    }
    let arg = inner[..close].trim();
    parse_quoted_js_string(arg)
}

/// Parse a quoted JS string literal using `'`, `"`, or `` ` `` delimiters.
fn parse_quoted_js_string(s: &str) -> Option<String> {
    let mut chars = s.chars();
    let quote = chars.next()?;
    if !matches!(quote, '\'' | '"' | '`') || !s.ends_with(quote) {
        return None;
    }
    let start = quote.len_utf8();
    let end = s.len().checked_sub(quote.len_utf8())?;
    Some(s[start..end].to_string())
}

/// Parse `exports.name = value` assignments.
fn parse_exports_assignment(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("exports.")?;
    let (name, rhs) = rest.split_once('=')?;
    let name = name.trim();
    if !is_valid_js_identifier(name) {
        return None;
    }
    let value = rhs.trim().trim_end_matches(';').trim();
    if value.is_empty() {
        return None;
    }
    Some((name.to_string(), value.to_string()))
}

fn is_identity_export(name: &str, value: &str) -> bool {
    if value == name {
        return true;
    }
    if let Some(trailing) = value.strip_prefix(name) {
        let trailing = trailing.trim_start();
        return trailing.starts_with("//") || trailing.starts_with("/*");
    }
    false
}

fn is_valid_js_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let valid_start = first == '_' || first == '$' || first.is_ascii_alphabetic();
    valid_start && chars.all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric())
}

fn read_feature_names(crate_dir: &Path) -> Result<std::collections::HashSet<String>, String> {
    let cargo_toml_path = crate_dir.join("Cargo.toml");
    let raw = std::fs::read_to_string(&cargo_toml_path)
        .map_err(|e| format!("Failed to read {}: {e}", cargo_toml_path.display()))?;
    let doc: toml::Value = raw
        .parse()
        .map_err(|e| format!("Failed to parse {}: {e}", cargo_toml_path.display()))?;

    Ok(doc
        .get("features")
        .and_then(|v| v.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default())
}

fn run_cargo_test_codegen(
    crate_dir: &Path,
    pkg_name: &str,
    out_dir: &Path,
    want_dts: bool,
    want_jsflow: bool,
) -> Result<(), String> {
    let available = read_feature_names(crate_dir)?;
    let requested = codegen_features_for_request(&available, pkg_name, want_dts, want_jsflow)?;

    let feature_arg = requested.join(",");
    let status = Command::new("cargo")
        .args([
            "test",
            "-p",
            pkg_name,
            "--features",
            &feature_arg,
            "--",
            "generate_npm_files",
        ])
        .current_dir(crate_dir)
        // Pass out_dir so bundle! macro writes .d.ts/.js.flow to the right place.
        .env("WJB_OUT_DIR", out_dir)
        .status()
        .map_err(|e| format!("Failed to run cargo test: {e}"))?;

    if !status.success() {
        return Err(format!("Codegen test failed with {status}"));
    }
    Ok(())
}

fn codegen_features_for_request(
    available: &std::collections::HashSet<String>,
    pkg_name: &str,
    want_dts: bool,
    want_jsflow: bool,
) -> Result<Vec<&'static str>, String> {
    let mut requested = vec!["codegen"];
    if want_dts {
        requested.push("ts");
    }
    if want_jsflow {
        requested.push("flow");
    }

    let missing: Vec<&str> = requested
        .iter()
        .copied()
        .filter(|feat| !available.contains(*feat))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "Cannot generate npm declaration files for {pkg_name}: missing Cargo feature(s): {}",
            missing.join(", ")
        ));
    }

    Ok(requested)
}

// ---------------------------------------------------------------------------
// package.json generation
// ---------------------------------------------------------------------------

/// A npm dep entry: name → version string (e.g. `"workspace:*"` or `"^0.1.0"`).
type NpmDeps = std::collections::BTreeMap<String, String>;

/// Run `cargo metadata` and return the raw JSON value.
fn cargo_metadata(crate_dir: &Path) -> Result<serde_json::Value, String> {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1"])
        .current_dir(crate_dir)
        .output()
        .map_err(|e| format!("Failed to run cargo metadata: {e}"))?;

    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    serde_json::from_slice(&out.stdout).map_err(|e| format!("Failed to parse cargo metadata: {e}"))
}

fn is_normal_dep(dep: &serde_json::Value) -> bool {
    dep["dep_kinds"]
        .as_array()
        .map(|kinds| kinds.iter().any(|k| k["kind"].is_null()))
        .unwrap_or(false)
}

/// Walk the cargo metadata graph and return npm dependencies for `pkg_name`.
///
/// For each direct normal dependency of `pkg_name`:
/// - If it has `[package.metadata.wasm-js-bridge].npm_name` → it produces an
///   npm package. All deps use `"^{version}"`.
/// - If it has no `npm_name` → pure Rust, already compiled into the WASM binary.
///   No npm dep needed.
fn resolve_npm_deps(metadata: &serde_json::Value, pkg_name: &str) -> Result<NpmDeps, String> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata missing packages")?;

    // Build id → package lookup
    let by_id: std::collections::HashMap<&str, &serde_json::Value> = packages
        .iter()
        .filter_map(|p| p["id"].as_str().map(|id| (id, p)))
        .collect();

    // Find the current package
    let current = packages
        .iter()
        .find(|p| p["name"].as_str() == Some(pkg_name))
        .ok_or_else(|| format!("Package {pkg_name} not found in cargo metadata"))?;

    let current_id = current["id"].as_str().ok_or("Package missing id")?;

    // Find resolve node for current package
    let node = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata missing resolve.nodes")?
        .iter()
        .find(|n| n["id"].as_str() == Some(current_id))
        .ok_or_else(|| format!("No resolve node for {pkg_name}"))?;

    let mut npm_deps = NpmDeps::new();

    if let Some(deps) = node["deps"].as_array() {
        for dep in deps {
            if !is_normal_dep(dep) {
                continue;
            }

            let dep_id = dep["pkg"].as_str().unwrap_or_default();
            let dep_pkg = match by_id.get(dep_id) {
                Some(p) => *p,
                None => continue,
            };

            // Check if this dep has an npm_name
            let npm_name = dep_pkg["metadata"]["wasm-js-bridge"]["npm_name"].as_str();
            if let Some(npm_name) = npm_name {
                let version = format!("^{}", dep_pkg["version"].as_str().unwrap_or("0.0.0"));
                npm_deps.insert(npm_name.to_string(), version);
            }
        }
    }

    Ok(npm_deps)
}

/// Generate `package.json` into `out_dir`.
///
/// The `exports` map is subpath → stem. If empty, defaults to `{ ".": stem }`.
/// Each stem is expanded to the full condition object:
/// ```json
/// { "import": "./{stem}.js", "require": "./{stem}.cjs", "types": "./{stem}.d.ts" }
/// ```
///
/// There is no standard package.json field for `.js.flow` declarations —
/// Flow type resolution uses `flow-typed/` stubs or `[ignore]` config instead.
/// The `.js.flow` file is emitted alongside the others for Flow users to consume
/// directly, but is not referenced in package.json.
fn generate_package_json(
    pkg_name: &str,
    stem: &str,
    meta: &WjbMeta,
    out_dir: &Path,
    cargo_meta: &serde_json::Value,
    log: &logger::Logger,
) -> Result<(), String> {
    let npm_name = match &meta.npm_name {
        Some(n) => n.clone(),
        None => {
            log.warn("Skipping package.json — no npm_name in [package.metadata.wasm-js-bridge].");
            return Ok(());
        }
    };

    let version = cargo_meta["packages"]
        .as_array()
        .and_then(|pkgs| pkgs.iter().find(|p| p["name"].as_str() == Some(pkg_name)))
        .and_then(|p| p["version"].as_str())
        .unwrap_or("0.0.0")
        .to_string();

    let npm_deps = resolve_npm_deps(cargo_meta, pkg_name)?;

    // Build the exports map: subpath → stem.
    let exports = resolved_exports(meta);
    let export_stems: Vec<(String, String)> = exports
        .iter()
        .map(|(subpath, src_file)| (subpath.clone(), file_to_stem(src_file)))
        .collect();

    // primary_stem is passed in by main (derived from "." export or first).
    let primary_stem = stem;

    let mut pkg = serde_json::Map::new();
    pkg.insert("name".to_string(), serde_json::json!(npm_name));
    pkg.insert("version".to_string(), serde_json::json!(version));
    pkg.insert("type".to_string(), serde_json::json!("module"));

    // Full conditional exports for both CJS+ESM and ESM-only packages.
    let mut exports_obj = serde_json::Map::new();
    for (subpath, export_stem) in &export_stems {
        let mut conditions = serde_json::Map::new();
        // types first — resolvers check it before runtime conditions.
        if meta.dts {
            conditions.insert(
                "types".to_string(),
                serde_json::json!(format!("./{export_stem}.{EXT_DTS}")),
            );
        }
        if meta.jsflow {
            conditions.insert(
                "flow".to_string(),
                serde_json::json!(format!("./{export_stem}.{EXT_FLOW}")),
            );
        }
        if meta.cjs {
            conditions.insert(
                "require".to_string(),
                serde_json::json!(format!("./{export_stem}.{EXT_CJS}")),
            );
        }
        conditions.insert(
            "import".to_string(),
            serde_json::json!(format!("./{export_stem}.{EXT_ESM}")),
        );
        exports_obj.insert(subpath.clone(), serde_json::Value::Object(conditions));
    }
    pkg.insert(
        "exports".to_string(),
        serde_json::Value::Object(exports_obj),
    );

    if !meta.cjs {
        // ESM-only compatibility fields for older tooling.
        pkg.insert(
            "main".to_string(),
            serde_json::json!(format!("./{primary_stem}.{EXT_ESM}")),
        );
        if meta.dts {
            pkg.insert(
                "types".to_string(),
                serde_json::json!(format!("./{primary_stem}.{EXT_DTS}")),
            );
        }
        if meta.jsflow {
            pkg.insert(
                "flow".to_string(),
                serde_json::json!(format!("./{primary_stem}.{EXT_FLOW}")),
            );
        }
    }
    pkg.insert(
        "engines".to_string(),
        serde_json::json!({ "node": ">=18.0.0" }),
    );

    // Collect all generated filenames across all exports, preserving insertion order.
    let mut files: Vec<serde_json::Value> = Vec::new();
    let mut seen_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push_file = |file: String| {
        if seen_files.insert(file.clone()) {
            files.push(serde_json::json!(file));
        }
    };

    push_file("package.json".to_string());
    for (_, s) in &export_stems {
        push_file(format!("{s}.{EXT_ESM}"));
        if meta.cjs {
            push_file(format!("{s}.{EXT_CJS}"));
        }
        if meta.dts {
            push_file(format!("{s}.{EXT_DTS}"));
        }
        if meta.jsflow {
            push_file(format!("{s}.{EXT_FLOW}"));
        }
    }
    pkg.insert("files".to_string(), serde_json::Value::Array(files));

    let mut scripts = serde_json::Map::new();
    scripts.insert(
        "prepack".to_string(),
        serde_json::json!("wasm-js-bridge build-workspace"),
    );
    pkg.insert("scripts".to_string(), serde_json::Value::Object(scripts));

    let license = cargo_meta["packages"]
        .as_array()
        .and_then(|pkgs| pkgs.iter().find(|p| p["name"].as_str() == Some(pkg_name)))
        .and_then(|p| p["license"].as_str())
        .unwrap_or("MIT")
        .to_string();
    pkg.insert("license".to_string(), serde_json::json!(license));

    let description = cargo_meta["packages"]
        .as_array()
        .and_then(|pkgs| pkgs.iter().find(|p| p["name"].as_str() == Some(pkg_name)))
        .and_then(|p| p["description"].as_str())
        .unwrap_or("")
        .to_string();
    if !description.is_empty() {
        pkg.insert("description".to_string(), serde_json::json!(description));
    }

    let repository = cargo_meta["packages"]
        .as_array()
        .and_then(|pkgs| pkgs.iter().find(|p| p["name"].as_str() == Some(pkg_name)))
        .and_then(|p| p["repository"].as_str())
        .unwrap_or("")
        .to_string();
    if !repository.is_empty() {
        pkg.insert("repository".to_string(), serde_json::json!(repository));
    }

    // Monorepo sibling packages are peers: the WASM binary is self-contained
    // (they are compiled in for Rust type use), but npm should not try to
    // deduplicate or bundle them — consumers install them independently.
    if !npm_deps.is_empty() {
        pkg.insert(
            "peerDependencies".to_string(),
            serde_json::to_value(&npm_deps).unwrap(),
        );
    }

    let json = serde_json::to_string_pretty(&serde_json::Value::Object(pkg))
        .map_err(|e| format!("Failed to serialize package.json: {e}"))?;

    std::fs::create_dir_all(out_dir)
        .map_err(|e| format!("Failed to create {}: {e}", out_dir.display()))?;
    std::fs::write(out_dir.join("package.json"), format!("{json}\n"))
        .map_err(|e| format!("Failed to write package.json: {e}"))
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

/// Warn if any npm-packaged workspace dep appears in the crate's unconditional
/// `[dependencies]` table.
///
/// Such deps will be statically linked into the WASM binary, making the peer
/// import shim ineffective. They should be moved to
/// `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`.
fn check_peer_deps_not_unconditional(
    crate_dir: &Path,
    pkg_name: &str,
    npm_pkgs: &std::collections::HashMap<&str, (&str, String, PathBuf, WjbMeta)>,
    log: &logger::Logger,
) {
    let cargo_toml_path = crate_dir.join("Cargo.toml");
    let raw = match std::fs::read_to_string(&cargo_toml_path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let doc: toml::Value = match raw.parse() {
        Ok(v) => v,
        Err(_) => return,
    };

    let unconditional_deps = match doc.get("dependencies").and_then(|v| v.as_table()) {
        Some(t) => t
            .iter()
            .map(|(dep_key, dep_val)| {
                let package_name = dep_val
                    .as_table()
                    .and_then(|tbl| tbl.get("package"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(dep_key)
                    .to_string();
                (dep_key.to_string(), package_name)
            })
            .collect::<Vec<_>>(),
        None => return,
    };

    // Build set of cargo package names that have npm_name (i.e. are npm-packaged).
    let npm_pkg_names: std::collections::HashSet<&str> =
        npm_pkgs.values().map(|(name, _, _, _)| *name).collect();

    for (dep_name, package_name) in &unconditional_deps {
        // Cargo dep names use hyphens; match against pkg names directly.
        let normalized = package_name.replace('_', "-");
        if npm_pkg_names.contains(normalized.as_str())
            || npm_pkg_names.contains(package_name.as_str())
        {
            let dep_hint = if dep_name != package_name {
                format!("{} (package = \"{}\")", dep_name, package_name)
            } else {
                dep_name.to_string()
            };
            log.warn(&format!(
                "\"{}\" is a regular [dependencies] entry in {} — it will be statically linked \
                into the WASM binary, making the peer import shim ineffective. Move it to \
                [target.'cfg(not(target_arch = \"wasm32\"))'.dependencies].",
                dep_hint, pkg_name
            ));
        }
    }
}

/// Build all npm-packaged workspace crates in topological order, passing
/// parsed WASM exports from each package to its dependents as peer import shims.
///
/// This is the webpack equivalent: outputs from earlier builds feed directly
/// into the import generation for later builds — no intermediate files committed.
fn build_workspace(args: BuildWorkspaceArgs, log: &logger::Logger) {
    let die = |msg: &str| -> ! {
        log.error(msg);
        std::process::exit(1);
    };

    // Discover workspace root and all members.
    let workspace_dir = std::env::current_dir()
        .unwrap_or_else(|e| die(&format!("Failed to get current directory: {e}")));

    let cargo_meta = cargo_metadata(&workspace_dir).unwrap_or_else(|e| die(&e));

    // Collect all workspace packages that have npm_name.
    let packages = cargo_meta["packages"]
        .as_array()
        .unwrap_or_else(|| die("cargo metadata missing packages"));

    let workspace_members: std::collections::HashSet<&str> = cargo_meta["workspace_members"]
        .as_array()
        .unwrap_or_else(|| die("cargo metadata missing workspace_members"))
        .iter()
        .filter_map(|v| v.as_str())
        .collect();

    // pkg_id → (pkg_name, npm_name, manifest_path, meta)
    let mut npm_pkgs: std::collections::HashMap<&str, (&str, String, PathBuf, WjbMeta)> =
        std::collections::HashMap::new();

    for pkg in packages {
        let id = pkg["id"].as_str().unwrap_or_default();
        if !workspace_members.contains(id) {
            continue;
        }

        let npm_name = match pkg["metadata"]["wasm-js-bridge"]["npm_name"].as_str() {
            Some(n) => n.to_string(),
            None => continue,
        };

        let pkg_name = pkg["name"].as_str().unwrap_or_default();
        let manifest = PathBuf::from(pkg["manifest_path"].as_str().unwrap_or_default());
        let crate_dir = manifest.parent().unwrap_or(&manifest).to_path_buf();

        let meta = read_meta(&crate_dir).unwrap_or_else(|e| die(&e)).1;

        npm_pkgs.insert(id, (pkg_name, npm_name, crate_dir, meta));
    }

    // Topological sort: build deps before dependents.
    // Walk resolve graph, emit packages with no unbuilt npm deps first.
    let nodes = cargo_meta["resolve"]["nodes"]
        .as_array()
        .unwrap_or_else(|| die("cargo metadata missing resolve.nodes"));

    let mut order: Vec<&str> = Vec::new();
    let node_by_id: std::collections::HashMap<&str, &serde_json::Value> = nodes
        .iter()
        .filter_map(|n| n["id"].as_str().map(|id| (id, n)))
        .collect();
    let mut pending: std::collections::BTreeSet<&str> = npm_pkgs.keys().copied().collect();

    // Deterministic topo sort over npm-packaged crates.
    while !pending.is_empty() {
        let before = pending.len();
        let ids: Vec<&str> = pending.iter().copied().collect();
        for id in ids {
            let node = node_by_id
                .get(id)
                .copied()
                .unwrap_or_else(|| die(&format!("Missing resolve node for package id {id}")));

            let mut blocked = false;
            if let Some(deps) = node["deps"].as_array() {
                for dep in deps {
                    if !is_normal_dep(dep) {
                        continue;
                    }
                    let dep_id = dep["pkg"].as_str().unwrap_or_default();
                    if npm_pkgs.contains_key(dep_id) && pending.contains(dep_id) {
                        blocked = true;
                        break;
                    }
                }
            }

            if !blocked {
                pending.remove(id);
                order.push(id);
            }
        }
        if pending.len() == before {
            die("Dependency cycle detected among npm-packaged workspace crates.");
        }
    }

    // exported_js[pkg_id] = (wasm-bindgen CJS JS output, WASM bytes) for peer shim generation.
    let mut exported_js: std::collections::HashMap<&str, (String, Vec<u8>)> =
        std::collections::HashMap::new();

    for &id in &order {
        let (pkg_name, npm_name, crate_dir, meta) = &npm_pkgs[id];
        let (pkg_name, npm_name, crate_dir) = (*pkg_name, npm_name.as_str(), crate_dir.as_path());
        let pkg_log = log.child(Box::leak(pkg_name.to_string().into_boxed_str()));

        let out_dir = args
            .out_dir
            .as_deref()
            .map(|d| d.join(pkg_name))
            .unwrap_or_else(|| crate_dir.to_path_buf());

        // Generate peer shim from all npm-packaged deps that have been built.
        let mut shim_content = String::new();
        let mut shim_mod_idx = 0usize;
        let node = node_by_id
            .get(id)
            .copied()
            .unwrap_or_else(|| die(&format!("Missing resolve node for package id {id}")));
        if let Some(deps) = node["deps"].as_array() {
            for dep in deps {
                if !is_normal_dep(dep) {
                    continue;
                }
                let dep_id = dep["pkg"].as_str().unwrap_or_default();
                if let Some((js, wasm_bytes)) = exported_js.get(dep_id) {
                    let (_, dep_npm_name, _, _) = &npm_pkgs[dep_id];
                    let exports = peer::merge_wasm_exports(peer::parse_exports(js), wasm_bytes);
                    let module_name = format!("__wjb_peers_dep_{shim_mod_idx}");
                    shim_mod_idx += 1;
                    shim_content.push_str(&peer::generate_peer_shim_with_module(
                        &exports,
                        dep_npm_name,
                        &module_name,
                    ));
                }
            }
        }

        // Write shim to tempfile; cargo build reads it via WJB_PEER_SHIM.
        let shim_path = if shim_content.is_empty() {
            None
        } else {
            let path = std::env::temp_dir().join(format!(
                "wjb-peers-{}-{}.rs",
                std::process::id(),
                pkg_name
            ));
            std::fs::write(&path, &shim_content)
                .unwrap_or_else(|e| die(&format!("Failed to write peer shim: {e}")));
            Some(path)
        };

        // Warn if any npm-packaged dep appears in unconditional [dependencies].
        // Such deps will be statically linked into the WASM binary, making the
        // peer import shim ineffective.
        check_peer_deps_not_unconditional(crate_dir, pkg_name, &npm_pkgs, &pkg_log);

        pkg_log.step("Compiling to WASM…");
        let wasm_path = cargo_build_wasm(
            crate_dir,
            pkg_name,
            &meta.wasm_features,
            shim_path.as_deref(),
        )
        .unwrap_or_else(|e| die(&e));

        // Cleanup shim tempfile immediately after build.
        if let Some(p) = &shim_path {
            let _ = std::fs::remove_file(p);
        }

        std::fs::create_dir_all(&out_dir)
            .unwrap_or_else(|e| die(&format!("Failed to create {}: {e}", out_dir.display())));

        let exports = resolved_exports(meta);
        let primary_stem = exports
            .get(".")
            .map(|s| file_to_stem(s))
            .unwrap_or_else(|| {
                file_to_stem(exports.values().next().unwrap_or(&"src/lib.rs".to_string()))
            });

        // Build each configured export stem from the same wasm binary.
        let mut peer_seed: Option<(String, Vec<u8>)> = None;
        for (subpath, src_file) in &exports {
            let stem = file_to_stem(src_file);
            let export_log =
                pkg_log.child(Box::leak(format!("{subpath} → {stem}").into_boxed_str()));

            export_log.step("Running wasm-bindgen…");
            let (js, wasm_bytes) = run_wasm_bindgen(&wasm_path, &stem).unwrap_or_else(|e| die(&e));

            if peer_seed.is_none() {
                peer_seed = Some((js.clone(), wasm_bytes.clone()));
            }

            if meta.cjs {
                export_log.step(&format!(
                    "Generating ESM + CJS ({stem}.{EXT_ESM}, {stem}.{EXT_CJS})…"
                ));
                std::thread::scope(|s| {
                    let esm = s.spawn(|| write_esm(&js, &wasm_bytes, &out_dir, &stem));
                    let cjs = s.spawn(|| write_cjs(&js, &wasm_bytes, &out_dir, &stem));
                    esm.join()
                        .unwrap_or_else(|_| Err("ESM thread panicked".into()))
                        .unwrap_or_else(|e| die(&e));
                    cjs.join()
                        .unwrap_or_else(|_| Err("CJS thread panicked".into()))
                        .unwrap_or_else(|e| die(&e));
                });
            } else {
                export_log.step(&format!("Generating ESM ({stem}.{EXT_ESM})…"));
                write_esm(&js, &wasm_bytes, &out_dir, &stem).unwrap_or_else(|e| die(&e));
            }
        }

        // Stash JS + WASM bytes for downstream peer shim generation.
        if let Some(seed) = peer_seed {
            exported_js.insert(id, seed);
        }

        if meta.dts || meta.jsflow {
            pkg_log.step("Generating type declarations…");
            run_cargo_test_codegen(crate_dir, pkg_name, &out_dir, meta.dts, meta.jsflow)
                .unwrap_or_else(|e| die(&e));
        }

        let cargo_meta_val = cargo_metadata(crate_dir).unwrap_or_else(|e| die(&e));
        pkg_log.step("Generating package.json…");
        generate_package_json(
            pkg_name,
            &primary_stem,
            meta,
            &out_dir,
            &cargo_meta_val,
            &pkg_log,
        )
        .unwrap_or_else(|e| die(&e));

        pkg_log.done(&format!("{npm_name} complete."));
    }
}

fn main() {
    logger::init();
    let log = logger::Logger::new("wasm-js-bridge");

    let cli = Cli::parse();

    if let Cli::BuildWorkspace(args) = cli {
        build_workspace(args, &log);
        return;
    }

    let Cli::Build(args) = cli else {
        unreachable!()
    };

    let crate_dir = std::fs::canonicalize(&args.path).unwrap_or_else(|e| {
        log.error(&format!("Invalid path {}: {e}.", args.path.display()));
        std::process::exit(1);
    });

    let (pkg_name, meta) = read_meta(&crate_dir).unwrap_or_else(|e| {
        log.error(&e);
        std::process::exit(1);
    });

    let out_dir = args.out_dir.unwrap_or_else(|| crate_dir.clone());
    let exports = resolved_exports(&meta);

    // CLI flags layer on top of metadata. ESM (.js) is always produced.
    let want_cjs = args.all || args.cjs || meta.cjs;
    let want_dts = args.all || args.dts || meta.dts;
    let want_jsflow = args.all || args.flow || meta.jsflow;

    let log = log.child(Box::leak(pkg_name.clone().into_boxed_str()));
    log.step(&format!("Building {} export(s)…", exports.len()));

    let die = |msg: &str| -> ! {
        log.error(msg);
        std::process::exit(1);
    };

    // Prefetch cargo metadata on a background thread — it's read-only and fast,
    // so it can overlap with the (potentially slow) WASM compile below.
    let meta_crate_dir = crate_dir.clone();
    let cargo_meta_thread = std::thread::spawn(move || cargo_metadata(&meta_crate_dir));

    // Validate wasm-bindgen CLI version against Cargo.lock before compiling.
    if let Some(required) = find_wasm_bindgen_version(&crate_dir) {
        check_wasm_bindgen_version(&required, &log);
    }

    // Compile to WASM once for the whole crate. This is the slow step.
    log.step("Compiling to WASM…");
    let wasm_path = match cargo_build_wasm(&crate_dir, &pkg_name, &meta.wasm_features, None) {
        Ok(p) => p,
        Err(e) => die(&e),
    };

    std::fs::create_dir_all(&out_dir)
        .unwrap_or_else(|e| die(&format!("Failed to create {}: {e}", out_dir.display())));

    // For each export: run wasm-bindgen (one invocation per stem), then write
    // ESM + optionally CJS in parallel. All exports share the same .wasm binary.
    for (subpath, src_file) in &exports {
        let stem = file_to_stem(src_file);
        let export_log = log.child(Box::leak(format!("{subpath} → {stem}").into_boxed_str()));

        export_log.step("Running wasm-bindgen…");
        let (js, wasm_bytes) = match run_wasm_bindgen(&wasm_path, &stem) {
            Ok(pair) => pair,
            Err(e) => die(&e),
        };

        if want_cjs {
            export_log.step(&format!(
                "Generating ESM + CJS ({stem}.{EXT_ESM}, {stem}.{EXT_CJS})…"
            ));
            std::thread::scope(|s| {
                let esm = s.spawn(|| write_esm(&js, &wasm_bytes, &out_dir, &stem));
                let cjs = s.spawn(|| write_cjs(&js, &wasm_bytes, &out_dir, &stem));
                if let Err(e) = esm
                    .join()
                    .unwrap_or_else(|_| Err("ESM thread panicked".into()))
                {
                    die(&e);
                }
                if let Err(e) = cjs
                    .join()
                    .unwrap_or_else(|_| Err("CJS thread panicked".into()))
                {
                    die(&e);
                }
            });
        } else {
            export_log.step(&format!("Generating ESM ({stem}.{EXT_ESM})…"));
            if let Err(e) = write_esm(&js, &wasm_bytes, &out_dir, &stem) {
                die(&e);
            }
        }
    }

    if want_dts || want_jsflow {
        log.step("Generating type declarations…");
        if let Err(e) =
            run_cargo_test_codegen(&crate_dir, &pkg_name, &out_dir, want_dts, want_jsflow)
        {
            die(&e);
        }
    }

    // Collect prefetched metadata (likely already done by now).
    let cargo_meta = cargo_meta_thread
        .join()
        .unwrap_or_else(|_| Err("cargo metadata thread panicked".into()))
        .unwrap_or_else(|e| die(&e));

    log.step("Generating package.json…");
    // Primary stem is derived from the "." export, or the first export if absent.
    let primary_stem = exports
        .get(".")
        .map(|s| file_to_stem(s))
        .unwrap_or_else(|| file_to_stem(exports.values().next().unwrap()));
    if let Err(e) =
        generate_package_json(&pkg_name, &primary_stem, &meta, &out_dir, &cargo_meta, &log)
    {
        die(&e);
    }

    log.done("Build complete.");
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
        let result = cjs_to_esm(
            "const { TextDecoder, TextEncoder } = require(`util`);\n",
            "test",
        );

        // Assert
        assert!(
            result.contains("import { TextDecoder, TextEncoder } from 'util'"),
            "should convert destructured require: {result}"
        );
        assert!(
            !result.contains("require("),
            "no require should remain: {result}"
        );
    }

    #[test]
    fn cjs_to_esm_exports_dot() {
        // Arrange and Act
        let result = cjs_to_esm("exports.parseSelector = parseSelector;\n", "test");

        // Assert
        assert!(
            result.contains("export {"),
            "should emit named export: {result}"
        );
        assert!(
            result.contains("parseSelector"),
            "export should include name: {result}"
        );
        assert!(
            !result.contains("exports."),
            "exports. should be rewritten: {result}"
        );
    }

    #[test]
    fn cjs_to_esm_exports_identity_no_space() {
        // Arrange and Act
        let result = cjs_to_esm("function f() {}\nexports.f=f\n", "test");

        // Assert
        assert!(
            !result.contains("const f = f;"),
            "identity export should not rebind: {result}"
        );
        assert!(
            result.contains("export { f };"),
            "identity export should be emitted: {result}"
        );
    }

    #[test]
    fn cjs_to_esm_exports_wasm_binding_has_semicolon() {
        // Arrange and Act
        let result = cjs_to_esm("exports.parseSelector = wasm.parseSelector\n", "test");

        // Assert
        assert!(
            result.contains("const parseSelector = wasm.parseSelector;"),
            "renamed/indirect export should become a local binding with semicolon: {result}"
        );
        assert!(
            result.contains("export { parseSelector };"),
            "must export rewritten binding: {result}"
        );
    }

    #[test]
    fn cjs_to_esm_does_not_rewrite_chained_require() {
        // Arrange and Act
        let result = cjs_to_esm("const join = require('path').join;\n", "test");

        // Assert
        assert!(
            result.contains("const join = require('path').join;"),
            "non-exact require call should remain unchanged: {result}"
        );
        assert!(
            !result.contains("import * as join from 'path';"),
            "should not emit invalid import: {result}"
        );
    }

    #[test]
    fn cjs_to_esm_non_threaded_full() {
        // Arrange and Act
        let result = cjs_to_esm(WBG_CJS_NON_THREADED, "myCrate");

        // Assert
        assert!(
            result.contains("import { TextDecoder, TextEncoder }"),
            "util import converted"
        );
        assert!(!result.contains("require(`util`)"), "util require gone");
        assert!(
            result.contains("export {"),
            "exports.* converted to named export"
        );
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
        assert_eq!(
            file_to_stem("src/mod.rs"),
            "mod",
            "root mod.rs should remain mod"
        );
    }

    #[test]
    fn generate_package_json_cjs_jsflow_peer_deps() {
        // Arrange
        use std::collections::BTreeMap;

        let tmp = tempfile::tempdir().expect("tempdir");
        let out_dir = tmp.path().to_path_buf();

        // Minimal cargo metadata with one sibling dep that has npm_name.
        let cargo_meta = serde_json::json!({
            "packages": [
                {
                    "id": "pkg-a 0.1.0",
                    "name": "pkg-a",
                    "version": "0.1.0",
                    "license": "MIT",
                    "description": "",
                    "repository": "",
                    "metadata": {
                        "wasm-js-bridge": {
                            "npm_name": "@test/pkg-a",
                            "cjs": true,
                            "jsflow": true
                        }
                    }
                },
                {
                    "id": "pkg-b 0.1.0",
                    "name": "pkg-b",
                    "version": "0.1.0",
                    "license": "MIT",
                    "description": "",
                    "repository": "",
                    "metadata": {
                        "wasm-js-bridge": {
                            "npm_name": "@test/pkg-b"
                        }
                    }
                }
            ],
            "resolve": {
                "nodes": [
                    {
                        "id": "pkg-a 0.1.0",
                        "deps": [
                            {
                                "pkg": "pkg-b 0.1.0",
                                "dep_kinds": [{ "kind": null }]
                            }
                        ]
                    },
                    {
                        "id": "pkg-b 0.1.0",
                        "deps": []
                    }
                ]
            }
        });

        let meta = WjbMeta {
            npm_name: Some("@test/pkg-a".to_string()),
            cjs: true,
            dts: true,
            jsflow: true,
            exports: BTreeMap::new(),
            wasm_features: "--features wasm".to_string(),
        };
        let log = logger::Logger::new("test");

        // Act
        generate_package_json("pkg-a", "lib", &meta, &out_dir, &cargo_meta, &log)
            .expect("generate_package_json should succeed");

        // Assert
        let content = std::fs::read_to_string(out_dir.join("package.json"))
            .expect("package.json should exist");
        let parsed: serde_json::Value =
            serde_json::from_str(&content).expect("package.json should be valid JSON");

        // exports["."] must have types, flow, require, import keys in that order.
        let dot_export = &parsed["exports"]["."];
        let keys: Vec<&str> = dot_export
            .as_object()
            .expect("exports[\".\"] must be an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            vec!["types", "flow", "require", "import"],
            "exports[\".\"] keys must be in order: types, flow, require, import"
        );

        // peerDependencies must be present (not dependencies).
        assert!(
            parsed.get("peerDependencies").is_some(),
            "peerDependencies must be present"
        );
        assert!(
            parsed.get("dependencies").is_none(),
            "dependencies must not be present; use peerDependencies"
        );
        assert_eq!(
            parsed["peerDependencies"]["@test/pkg-b"], "^0.1.0",
            "peerDependency version should be ^version"
        );

        // files must contain the right filenames.
        let files: Vec<&str> = parsed["files"]
            .as_array()
            .expect("files must be an array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            files.contains(&"package.json"),
            "files must include package.json"
        );
        assert!(files.contains(&"lib.js"), "files must include lib.js");
        assert!(files.contains(&"lib.cjs"), "files must include lib.cjs");
        assert!(files.contains(&"lib.d.ts"), "files must include lib.d.ts");
        assert!(
            files.contains(&"lib.js.flow"),
            "files must include lib.js.flow"
        );

        // prepack must use build-workspace.
        assert_eq!(
            parsed["scripts"]["prepack"], "wasm-js-bridge build-workspace",
            "prepack must use build-workspace"
        );
    }

    #[test]
    fn generate_package_json_esm_only_multi_export() {
        // Arrange
        use std::collections::BTreeMap;

        let tmp = tempfile::tempdir().expect("tempdir");
        let out_dir = tmp.path().to_path_buf();

        let cargo_meta = serde_json::json!({
            "packages": [
                {
                    "id": "pkg-a 0.1.0",
                    "name": "pkg-a",
                    "version": "0.1.0",
                    "license": "MIT",
                    "description": "",
                    "repository": "",
                    "metadata": {
                        "wasm-js-bridge": {
                            "npm_name": "@test/pkg-a"
                        }
                    }
                }
            ],
            "resolve": {
                "nodes": [
                    { "id": "pkg-a 0.1.0", "deps": [] }
                ]
            }
        });

        let mut exports = BTreeMap::new();
        exports.insert(".".to_string(), "src/lib.rs".to_string());
        exports.insert("./util".to_string(), "src/foo_bar.rs".to_string());

        let meta = WjbMeta {
            npm_name: Some("@test/pkg-a".to_string()),
            cjs: false,
            dts: true,
            jsflow: false,
            exports,
            wasm_features: "--features wasm".to_string(),
        };
        let log = logger::Logger::new("test");

        // Act
        generate_package_json("pkg-a", "lib", &meta, &out_dir, &cargo_meta, &log)
            .expect("generate_package_json should succeed");

        // Assert
        let content = std::fs::read_to_string(out_dir.join("package.json"))
            .expect("package.json should exist");
        let parsed: serde_json::Value =
            serde_json::from_str(&content).expect("package.json should be valid JSON");

        assert_eq!(
            parsed["type"], "module",
            "esm-only package must still set type=module"
        );
        assert_eq!(
            parsed["main"], "./lib.js",
            "main should point at primary stem"
        );
        assert_eq!(
            parsed["types"], "./lib.d.ts",
            "types should point at primary stem"
        );
        assert!(
            parsed["exports"]["."]["import"] == "./lib.js"
                && parsed["exports"]["./util"]["import"] == "./fooBar.js",
            "exports map should include all subpaths for ESM-only mode"
        );
        assert!(
            parsed["exports"]["."].get("require").is_none()
                && parsed["exports"]["./util"].get("require").is_none(),
            "ESM-only exports should not include require condition"
        );
    }

    #[test]
    fn codegen_features_match_requested_outputs() {
        // Arrange
        let available: std::collections::HashSet<String> = ["codegen", "ts", "flow"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Act
        let ts_only = codegen_features_for_request(&available, "pkg-a", true, false).unwrap();
        let flow_only = codegen_features_for_request(&available, "pkg-a", false, true).unwrap();
        let both = codegen_features_for_request(&available, "pkg-a", true, true).unwrap();

        // Assert
        assert_eq!(
            ts_only,
            vec!["codegen", "ts"],
            "d.ts-only should not require flow"
        );
        assert_eq!(
            flow_only,
            vec!["codegen", "flow"],
            "flow-only should not require ts"
        );
        assert_eq!(
            both,
            vec!["codegen", "ts", "flow"],
            "both outputs require all features"
        );
    }

    #[test]
    fn codegen_features_missing_requested_feature_errors() {
        // Arrange
        let available: std::collections::HashSet<String> =
            ["codegen", "ts"].iter().map(|s| s.to_string()).collect();

        // Act
        let err = codegen_features_for_request(&available, "pkg-a", false, true)
            .expect_err("flow should be required when jsflow is requested");

        // Assert
        assert!(
            err.contains("missing Cargo feature(s): flow"),
            "error should name missing features: {err}"
        );
    }
}
