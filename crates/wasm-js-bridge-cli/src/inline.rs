//! WASM inlining — replaces the wasm-bindgen file-system WASM load with
//! base64-encoded inline bytes, producing a self-contained JS file with no
//! external `_bg.wasm` dependency.
//!
//! wasm-bindgen nodejs target generates one of two load patterns depending on
//! its version. We handle both:
//!
//! **Pattern A** (older):
//! ```js
//! const path = require('path').join(__dirname, 'stem_bg.wasm');
//! const bytes = require('fs').readFileSync(path);
//! ```
//!
//! **Pattern B** (newer, single expression):
//! ```js
//! require('fs').readFileSync(require('path').join(__dirname, 'stem_bg.wasm'))
//! ```
//!
//! Both are replaced with:
//! ```js
//! Buffer.from('BASE64', 'base64')
//! ```

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

    // Pattern A: two-line form
    //   const path = require('path').join(__dirname, '..._bg.wasm');
    //   const bytes = require('fs').readFileSync(path);
    let patched = replace_pattern_a(js, &replacement)
        .or_else(|| replace_pattern_b(js, &replacement));

    patched.ok_or_else(|| {
        "Could not find wasm-bindgen WASM load pattern in generated JS. \
         The wasm-bindgen-cli-support version may have changed its output format."
            .to_string()
    })
}

/// Inline `wasm_bytes` into the wasm-bindgen web/bundler JS output for ESM.
///
/// The ESM bundler target imports WASM as a module-level side-effecting import;
/// we switch to the nodejs target JS and replace the load with an inline
/// `Uint8Array` so the output is a self-contained ESM module.
///
/// The returned string replaces the CJS `Buffer.from` shim with a
/// `Uint8Array` that works in both browser and Node ESM contexts.
pub fn inline_wasm_esm(js: &str, wasm_bytes: &[u8]) -> Result<String, String> {
    let b64 = B64.encode(wasm_bytes);
    // Use atob for browser compat; Node 16+ has atob globally.
    let replacement = format!(
        "Uint8Array.from(atob('{b64}'), c => c.charCodeAt(0))"
    );

    let patched = replace_pattern_a(js, &replacement)
        .or_else(|| replace_pattern_b(js, &replacement));

    patched.ok_or_else(|| {
        "Could not find wasm-bindgen WASM load pattern in generated JS (ESM). \
         The wasm-bindgen-cli-support version may have changed its output format."
            .to_string()
    })
}

/// Replace pattern A:
/// ```js
/// const path = require('path').join(__dirname, '..._bg.wasm');\nconst bytes = require('fs').readFileSync(path);
/// ```
fn replace_pattern_a(js: &str, replacement: &str) -> Option<String> {
    // Find the path assignment line
    let path_line_start = js.find("const path = require('path').join(__dirname,")?;
    let path_line_end = js[path_line_start..].find('\n')? + path_line_start;

    // The bytes assignment must follow immediately (possibly after whitespace)
    let after_path = js[path_line_end + 1..].trim_start();
    if !after_path.starts_with("const bytes = require('fs').readFileSync(path)") {
        return None;
    }

    let bytes_line_start = js[path_line_end + 1..].find("const bytes = require('fs').readFileSync(path)")?
        + path_line_end + 1;
    let bytes_line_end = js[bytes_line_start..].find('\n').map(|i| i + bytes_line_start)
        .unwrap_or(js.len());

    let mut out = String::with_capacity(js.len());
    out.push_str(&js[..path_line_start]);
    out.push_str("const bytes = ");
    out.push_str(replacement);
    out.push(';');
    out.push_str(&js[bytes_line_end..]);
    Some(out)
}

/// Replace pattern B:
/// ```js
/// require('fs').readFileSync(require('path').join(__dirname, '..._bg.wasm'))
/// ```
fn replace_pattern_b(js: &str, replacement: &str) -> Option<String> {
    let needle = "require('fs').readFileSync(require('path').join(__dirname,";
    let start = js.find(needle)?;

    // Find the matching closing paren for readFileSync(...)
    let args_start = start + "require('fs').readFileSync(".len();
    let close = find_matching_paren(&js[args_start..])? + args_start;
    let end = close + 1; // include the closing ')'

    let mut out = String::with_capacity(js.len());
    out.push_str(&js[..start]);
    out.push_str(replacement);
    out.push_str(&js[end..]);
    Some(out)
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

    #[test]
    fn inline_cjs_pattern_a() {
        // Arrange
        let js = "let wasm;\nconst path = require('path').join(__dirname, 'foo_bg.wasm');\nconst bytes = require('fs').readFileSync(path);\nwasm = bytes;\n";

        // Act
        let result = inline_wasm_cjs(js, FAKE_WASM).unwrap();

        // Assert
        assert!(!result.contains("require('fs')"), "fs.readFileSync should be gone");
        assert!(!result.contains("require('path')"), "path.join should be gone");
        assert!(result.contains("Buffer.from("), "should contain inline bytes");
        assert!(result.contains("base64"), "should use base64 encoding");
    }

    #[test]
    fn inline_cjs_pattern_b() {
        // Arrange
        let js = "let wasm;\nconst bytes = require('fs').readFileSync(require('path').join(__dirname, 'foo_bg.wasm'));\nwasm = bytes;\n";

        // Act
        let result = inline_wasm_cjs(js, FAKE_WASM).unwrap();

        // Assert
        assert!(!result.contains("require('fs')"), "fs.readFileSync should be gone");
        assert!(result.contains("Buffer.from("), "should contain inline bytes");
    }

    #[test]
    fn inline_esm_uses_uint8array() {
        // Arrange
        let js = "const path = require('path').join(__dirname, 'foo_bg.wasm');\nconst bytes = require('fs').readFileSync(path);\n";

        // Act
        let result = inline_wasm_esm(js, FAKE_WASM).unwrap();

        // Assert
        assert!(result.contains("Uint8Array.from(atob("), "ESM should use Uint8Array + atob");
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
}
