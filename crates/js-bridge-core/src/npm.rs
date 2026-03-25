use std::collections::{BTreeMap, HashMap, HashSet};

/// A npm dep entry: name → version string (e.g. `"workspace:*"` or `"^0.1.0"`).
pub type NpmDeps = BTreeMap<String, String>;

pub fn is_normal_dep(dep: &serde_json::Value) -> bool {
    dep["dep_kinds"]
        .as_array()
        .map(|kinds| kinds.iter().any(|k| k["kind"].is_null()))
        .unwrap_or(false)
}

/// Walk the cargo metadata graph and return npm dependencies for `pkg_name`.
///
/// `meta_key` is the `[package.metadata.<key>]` section that carries `npm_name`
/// (e.g. `"wasm-js-bridge"` or `"native-js-bridge"`).
///
/// For each direct normal dependency of `pkg_name`:
/// - If it has `[package.metadata.<meta_key>].npm_name` → it produces an npm package.
///   - Same Cargo workspace → `"workspace:*"` (yarn berry and pnpm replace on publish;
///     npm does not support the workspace: protocol — use pnpm or yarn for this toolchain).
///   - External crate → `"^{version}"`.
/// - If it has no `npm_name` → pure Rust, compiled into the binary. No npm dep needed.
pub fn resolve_npm_deps(
    metadata: &serde_json::Value,
    pkg_name: &str,
    meta_key: &str,
) -> Result<NpmDeps, String> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata missing packages")?;

    // Build id → package lookup
    let by_id: HashMap<&str, &serde_json::Value> = packages
        .iter()
        .filter_map(|p| p["id"].as_str().map(|id| (id, p)))
        .collect();

    // Workspace member ids for workspace:* detection
    let workspace_members: HashSet<&str> = metadata["workspace_members"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

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

            let npm_name = dep_pkg["metadata"][meta_key]["npm_name"].as_str();
            if let Some(npm_name) = npm_name {
                let version = if workspace_members.contains(dep_id) {
                    "workspace:*".to_string()
                } else {
                    format!("^{}", dep_pkg["version"].as_str().unwrap_or("0.0.0"))
                };
                npm_deps.insert(npm_name.to_string(), version);
            }
        }
    }

    Ok(npm_deps)
}
