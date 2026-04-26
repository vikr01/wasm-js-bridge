//! Installs the TypeScript compiler via bun and copies it + lib.es5.d.ts to OUT_DIR.

use std::path::{Path, PathBuf};
use std::process::Command;

const JS_FILE: &str = "node_modules/typescript/lib/typescript.js";
const LIB_ES5_FILE: &str = "node_modules/typescript/lib/lib.es5.d.ts";

fn install_deps(crate_dir: &Path) {
    let status = Command::new("bun")
        .arg("install")
        .current_dir(crate_dir)
        .status()
        .unwrap_or_else(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                panic!("bun not found on PATH — install it: https://bun.sh");
            }
            panic!("failed to run `bun install`: {e}");
        });
    assert!(
        status.success(),
        "`bun install` failed with status {status}"
    );
}

fn main() {
    println!("cargo::rerun-if-changed=package.json");
    println!("cargo::rerun-if-changed={JS_FILE}");
    println!("cargo::rerun-if-changed={LIB_ES5_FILE}");

    let crate_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));

    let ts_src = crate_dir.join(JS_FILE);
    let lib_src = crate_dir.join(LIB_ES5_FILE);
    if !ts_src.exists() || !lib_src.exists() {
        install_deps(&crate_dir);
    }

    assert!(
        ts_src.exists(),
        "typescript.js not found at {} after install",
        ts_src.display()
    );
    assert!(
        lib_src.exists(),
        "lib.es5.d.ts not found at {} after install",
        lib_src.display()
    );

    std::fs::copy(&ts_src, out_dir.join("typescript.js"))
        .unwrap_or_else(|e| panic!("failed to copy typescript.js: {e}"));
    std::fs::copy(&lib_src, out_dir.join("lib.es5.d.ts"))
        .unwrap_or_else(|e| panic!("failed to copy lib.es5.d.ts: {e}"));
}
