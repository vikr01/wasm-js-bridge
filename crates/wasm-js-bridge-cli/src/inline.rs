//! WASM inlining — replaces the wasm-bindgen file-system WASM load with
//! base64-encoded inline bytes, producing a self-contained JS file with no
//! external `_bg.wasm` dependency.
//!
//! wasm-bindgen 0.2.114 nodejs (CJS) target generates one of two load patterns:
//!
//! **Pattern A** (non-threaded, two-statement form):
//! ```js
//! const wasmPath = `${__dirname}/stem_bg.wasm`;
//! const wasmBytes = require('fs').readFileSync(wasmPath);
//! ```
//!
//! **Pattern B** (threaded, inside `initSync` function):
//! ```js
//! const wasmPath = `${__dirname}/stem_bg.wasm`;
//! module = require('fs').readFileSync(wasmPath);
//! ```
//!
//! Both are replaced with an inline bytes expression so no external `.wasm`
//! file is needed at runtime.

use base64::Engine as _;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Inline `wasm_bytes` into the wasm-bindgen nodejs JS output.
///
/// Returns the patched JS string with all filesystem WASM loads replaced by
/// an inline `Buffer.from(...)` expression. Errors if no known load pattern
/// is found (likely a wasm-bindgen version we haven't seen before).
pub fn inline_wasm_cjs(js: &str, wasm_bytes: &[u8]) -> Result<String, String> {
    let b64 = B64.encode(wasm_bytes);
    let replacement = format!("Buffer.from('{b64}', 'base64')");
    patch_wasm_load(js, &replacement).ok_or_else(|| {
        "Could not find wasm-bindgen WASM load pattern in generated JS. \
         The wasm-bindgen-cli-support version may have changed its output format."
            .to_string()
    })
}

/// Inline `wasm_bytes` into the wasm-bindgen nodejs JS output for ESM wrapping.
///
/// Uses `Uint8Array.from(atob(...))` instead of `Buffer.from(...)` for
/// browser + Node ESM compatibility. Otherwise identical to `inline_wasm_cjs`.
pub fn inline_wasm_esm(js: &str, wasm_bytes: &[u8]) -> Result<String, String> {
    let b64 = B64.encode(wasm_bytes);
    let replacement = format!("Uint8Array.from(atob('{b64}'), c => c.charCodeAt(0))");
    patch_wasm_load(js, &replacement).ok_or_else(|| {
        "Could not find wasm-bindgen WASM load pattern in generated JS (ESM). \
         The wasm-bindgen-cli-support version may have changed its output format."
            .to_string()
    })
}

/// Try all known wasm-bindgen nodejs CJS load patterns in order, returning the
/// first successful replacement, or `None` if none matched.
fn patch_wasm_load(js: &str, replacement: &str) -> Option<String> {
    replace_pattern_a(js, replacement).or_else(|| replace_pattern_b(js, replacement))
}

/// Replace Pattern A — non-threaded two-statement form:
///
/// ```js
/// const wasmPath = `${__dirname}/…_bg.wasm`;
/// const wasmBytes = require('fs').readFileSync(wasmPath);
/// ```
///
/// The `wasmPath` line is dropped and the `wasmBytes` assignment is replaced
/// with the inline expression. Both single-quote and backtick variants of the
/// `require('fs')` call are handled, and leading whitespace before either line
/// is tolerated.
fn replace_pattern_a(js: &str, replacement: &str) -> Option<String> {
    // Locate the wasmPath assignment (template literal, may be indented)
    let path_marker_end = js.find("_bg.wasm")?;
    // Walk back to find the start of the statement
    let path_line_start = js[..path_marker_end]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);

    // The assignment target may be `wasmPath` or similar; we only require the
    // template literal containing `__dirname}/<something>_bg.wasm`.
    let path_line_end = js[path_line_start..]
        .find('\n')
        .map(|i| i + path_line_start)
        .unwrap_or(js.len());
    let path_line = &js[path_line_start..path_line_end];
    if !path_line.contains("__dirname}/") {
        return None;
    }
    if path_line_end >= js.len() {
        return None;
    }

    // The wasmBytes / module assignment must follow on the very next non-blank line.
    let after_path = &js[path_line_end + 1..];
    let (bytes_rel, bytes_needle) = find_fs_readfilesync(after_path)?;
    // The text before `require('fs')` on that line is the LHS assignment; there
    // must be no additional newlines containing non-whitespace content between
    // the path line and the require call (i.e. at most one intervening line).
    let interstitial = &after_path[..bytes_rel];
    let extra_line_content = interstitial
        .split('\n')
        .skip(1) // first segment is after path_line_end newline, on the same line as require
        .any(|seg| seg.chars().any(|c| !c.is_whitespace()));
    if extra_line_content {
        return None;
    }
    let bytes_start = path_line_end + 1 + bytes_rel;
    // Find the readFileSync(…) call; locate the opening paren
    let args_start_rel = bytes_needle.len();
    let args_start = bytes_start + args_start_rel;
    let close_rel = find_matching_paren(&js[args_start..])?;
    let bytes_call_end = args_start + close_rel + 1; // inclusive of ')'

    // Find the end of the statement (semicolon or end of line)
    let stmt_end = js[bytes_call_end..]
        .find([';', '\n'])
        .map(|i| bytes_call_end + i + 1)
        .unwrap_or(bytes_call_end);

    // Walk back from bytes_start to find the beginning of the statement's line
    let bytes_line_start = js[..bytes_start].rfind('\n').map(|i| i + 1).unwrap_or(0);

    // Reconstruct: keep everything before the path line, then replace both
    // lines with a single assignment using the inline expression.
    // Detect what LHS was used for the wasmBytes assignment.
    let lhs_region = &js[bytes_line_start..bytes_start];
    let lhs = lhs_region.trim();
    // `lhs` is e.g. `const wasmBytes =` or `module =`

    let mut out = String::with_capacity(js.len());
    out.push_str(&js[..path_line_start]);
    out.push_str(lhs);
    out.push(' ');
    out.push_str(replacement);
    out.push(';');
    out.push_str(&js[stmt_end..]);
    Some(out)
}

/// Replace Pattern B — legacy single-expression form:
///
/// ```js
/// require('fs').readFileSync(require('path').join(__dirname, '…_bg.wasm'))
/// ```
fn replace_pattern_b(js: &str, replacement: &str) -> Option<String> {
    let (start, needle) = find_fs_readfilesync(js)?;
    let args_start = start + needle.len();
    let close = find_matching_paren(&js[args_start..])? + args_start;
    let end = close + 1;

    let mut out = String::with_capacity(js.len());
    out.push_str(&js[..start]);
    out.push_str(replacement);
    out.push_str(&js[end..]);
    Some(out)
}

/// Find a `require("fs").readFileSync(` call and return `(byte_offset, needle)`.
fn find_fs_readfilesync(s: &str) -> Option<(usize, &'static str)> {
    const NEEDLES: [&str; 3] = [
        "require('fs').readFileSync(",
        "require(\"fs\").readFileSync(",
        "require(`fs`).readFileSync(",
    ];

    NEEDLES
        .iter()
        .filter_map(|needle| s.find(needle).map(|idx| (idx, *needle)))
        .min_by_key(|(idx, _)| *idx)
}

/// Find the index of the closing `)` that matches the opening `(` assumed to
/// be at the very start of `s` (i.e. `s` starts just after an opening `(`).
fn find_matching_paren(s: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE_WASM: &[u8] = b"\x00asm\x01\x00\x00\x00";

    // Snippet representative of wasm-bindgen 0.2.114 non-threaded CJS output.
    const WBG_NON_THREADED: &str = r#"
'use strict';

let wasm;

// ... bindings ...

const wasmPath = `${__dirname}/my_crate_bg.wasm`;
const wasmBytes = require('fs').readFileSync(wasmPath);
const wasmModule = new WebAssembly.Module(wasmBytes);
let instance = new WebAssembly.Instance(wasmModule, __wbg_get_imports()).exports;
wasm = instance;
"#;

    // Snippet representative of wasm-bindgen 0.2.114 threaded CJS output
    // (the readFileSync is inside initSync).
    const WBG_THREADED: &str = r#"
'use strict';

let wasm;
let wasmModule;
let memory;
let __initialized = false;

exports.initSync = function(opts) {
    if (opts === undefined) opts = {};
    if (__initialized) return wasm;

    let module = opts.module;

    if (module === undefined) {
        const wasmPath = `${__dirname}/my_crate_bg.wasm`;
        module = require('fs').readFileSync(wasmPath);
    }

    if (!(module instanceof WebAssembly.Module)) {
        wasmModule = new WebAssembly.Module(module);
    }
};

if (require('worker_threads').isMainThread) {
    exports.initSync();
}
"#;

    #[test]
    fn inline_cjs_non_threaded() {
        // Arrange and Act
        let result = inline_wasm_cjs(WBG_NON_THREADED, FAKE_WASM).unwrap();

        // Assert
        assert!(
            !result.contains("require('fs')"),
            "fs.readFileSync should be gone"
        );
        assert!(!result.contains("wasmPath"), "wasmPath var should be gone");
        assert!(
            result.contains("Buffer.from("),
            "should contain inline bytes"
        );
        assert!(result.contains("'base64'"), "should use base64 encoding");
    }

    #[test]
    fn inline_cjs_threaded() {
        // Arrange and Act
        let result = inline_wasm_cjs(WBG_THREADED, FAKE_WASM).unwrap();

        // Assert
        assert!(
            !result.contains("require('fs')"),
            "fs.readFileSync should be gone"
        );
        assert!(
            result.contains("Buffer.from("),
            "should contain inline bytes"
        );
        // The worker_threads require should still be present (different require)
        assert!(
            result.contains("require('worker_threads')"),
            "worker_threads require should remain"
        );
    }

    #[test]
    fn inline_esm_uses_uint8array() {
        // Arrange and Act
        let result = inline_wasm_esm(WBG_NON_THREADED, FAKE_WASM).unwrap();

        // Assert
        assert!(
            result.contains("Uint8Array.from(atob("),
            "ESM should use Uint8Array + atob"
        );
        assert!(
            !result.contains("Buffer.from("),
            "ESM should not use Buffer"
        );
    }

    #[test]
    fn unknown_pattern_errors() {
        // Arrange
        let js = "const bytes = fs.readFileSync('unknown_pattern.wasm');";

        // Act
        let result = inline_wasm_cjs(js, FAKE_WASM);

        // Assert
        assert!(result.is_err(), "unknown pattern should return an error");
    }

    // Legacy Pattern B — older wasm-bindgen single-expression form
    #[test]
    fn inline_cjs_legacy_pattern_b() {
        // Arrange
        let js = "let wasm;\nconst bytes = require('fs').readFileSync(require('path').join(__dirname, 'foo_bg.wasm'));\nwasm = bytes;\n";

        // Act
        let result = inline_wasm_cjs(js, FAKE_WASM).unwrap();

        // Assert
        assert!(
            !result.contains("require('fs')"),
            "fs.readFileSync should be gone"
        );
        assert!(
            result.contains("Buffer.from("),
            "should contain inline bytes"
        );
    }

    #[test]
    fn inline_cjs_handles_double_quote_require() {
        // Arrange
        let js = r#"
const wasmPath = `${__dirname}/foo_bg.wasm`;
const wasmBytes = require("fs").readFileSync(wasmPath);
"#;

        // Act
        let result = inline_wasm_cjs(js, FAKE_WASM).unwrap();

        // Assert
        assert!(
            !result.contains("require(\"fs\")"),
            "double-quote fs require should be rewritten"
        );
        assert!(result.contains("Buffer.from("), "should inline wasm bytes");
    }

    #[test]
    fn inline_cjs_handles_backtick_require() {
        // Arrange
        let js = r#"
const wasmPath = `${__dirname}/foo_bg.wasm`;
module = require(`fs`).readFileSync(wasmPath);
"#;

        // Act
        let result = inline_wasm_cjs(js, FAKE_WASM).unwrap();

        // Assert
        assert!(
            !result.contains("require(`fs`)"),
            "backtick fs require should be rewritten"
        );
        assert!(result.contains("Buffer.from("), "should inline wasm bytes");
    }
}
