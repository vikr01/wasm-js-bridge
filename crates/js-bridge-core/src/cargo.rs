use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Walk up from `crate_dir` to find the workspace `Cargo.lock`.
pub fn find_cargo_lock(crate_dir: &Path) -> Option<PathBuf> {
    let mut dir = crate_dir;
    loop {
        let candidate = dir.join("Cargo.lock");
        if candidate.exists() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
}

pub fn read_feature_names(crate_dir: &Path) -> Result<HashSet<String>, String> {
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

/// Run `cargo metadata` and return the raw JSON value.
pub fn cargo_metadata(crate_dir: &Path) -> Result<serde_json::Value, String> {
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
