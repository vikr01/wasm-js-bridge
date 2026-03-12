//! WASM peer import shim generation.
//!
//! Parses the JSDoc-annotated wasm-bindgen nodejs CJS output to extract every
//! exported function's name and type signature, then generates a Rust
//! `extern "C"` block that imports those functions from the npm package via
//! `#[wasm_bindgen(module = "...")]`. No intermediate files are committed —
//! the shim is written to a tempfile and deleted after the dependent build.
//!
//! Type mapping (JSDoc → Rust extern type):
//!
//! | JSDoc       | arg type  | return type |
//! |-------------|-----------|-------------|
//! | `string`    | `&str`    | `String`    |
//! | `boolean`   | `bool`    | `bool`      |
//! | `number`    | `f64`     | `f64`       |
//! | `bigint`    | `u64`     | `u64`       |
//! | `any` / *   | `JsValue` | `JsValue`   |
//! | `void`/none | —         | `()`        |

/// A single export extracted from wasm-bindgen JS output.
#[derive(Debug)]
pub struct Export {
    /// JavaScript name (camelCase), e.g. `"evalPredicate"`.
    pub js_name: String,
    /// Rust snake_case name derived from `js_name`.
    pub rust_name: String,
    /// Argument types in declaration order.
    pub args: Vec<ExportType>,
    /// Return type (`Void` for no return value).
    pub ret: ExportType,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExportType {
    Str,     // &str (arg) / String (return)
    JsValue, // JsValue
    F64,     // f64
    Bool,    // bool
    U64,     // u64 (bigint)
    Void,    // ()
}

impl ExportType {
    fn from_jsdoc(s: &str) -> Self {
        match s.trim().trim_matches(|c| c == '{' || c == '}') {
            "string" => Self::Str,
            "boolean" => Self::Bool,
            "number" => Self::F64,
            "bigint" => Self::U64,
            "void" | "undefined" | "null" => Self::Void,
            "any" | "*" => Self::JsValue,
            _ => Self::JsValue,
        }
    }

    fn as_arg_type(&self) -> &'static str {
        match self {
            Self::Str => "&str",
            Self::Bool => "bool",
            Self::F64 => "f64",
            Self::U64 => "u64",
            Self::JsValue | Self::Void => "JsValue",
        }
    }

    fn as_ret_type(&self) -> &'static str {
        match self {
            Self::Str => "String",
            Self::Bool => "bool",
            Self::F64 => "f64",
            Self::U64 => "u64",
            Self::JsValue => "JsValue",
            Self::Void => "()",
        }
    }
}

/// Convert a camelCase JS name to snake_case.
fn camel_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for (i, c) in s.char_indices() {
        if c.is_ascii_uppercase() && i != 0 {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

#[derive(Clone)]
struct DocSig {
    args: Vec<ExportType>,
    ret: ExportType,
}

struct ExportAssign {
    js_name: String,
    function_param_count: Option<usize>,
    value_ident: Option<String>,
}

/// Parse wasm-bindgen nodejs CJS output and return all exported functions.
///
/// Relies on the JSDoc comment block that wasm-bindgen emits above each
/// `exports.name = function(...)` assignment. Functions without JSDoc are
/// included with all-`JsValue` types as a safe fallback.
pub fn parse_exports(js: &str) -> Vec<Export> {
    let mut exports = Vec::new();
    let mut seen_exports: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut typed_locals: std::collections::HashMap<String, DocSig> =
        std::collections::HashMap::new();
    let lines: Vec<&str> = js.lines().collect();
    let mut pending_doc: Option<DocSig> = None;
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim();
        if trimmed == "/**" {
            let (doc, next_i) = parse_jsdoc(&lines, i + 1);
            pending_doc = Some(doc);
            i = next_i;
            continue;
        }

        if let Some(local_name) = extract_local_function_name(trimmed) {
            if let Some(doc) = pending_doc.take() {
                typed_locals.insert(local_name, doc);
            }
            i += 1;
            continue;
        }

        if let Some(assign) = parse_export_assignment(trimmed) {
            let export = build_export(&assign, pending_doc.as_ref(), &typed_locals);
            pending_doc = None;
            if seen_exports.insert(export.js_name.clone()) {
                exports.push(export);
            }
            i += 1;
            continue;
        }

        i += 1;
    }

    exports
}

/// Cross-reference JSDoc-parsed exports against WASM binary export names.
///
/// Functions present in the WASM exports but absent from the JSDoc parse result
/// are added with all-`JsValue` types as a safe fallback. Functions already
/// found via JSDoc are not modified.
pub fn merge_wasm_exports(mut exports: Vec<Export>, wasm_bytes: &[u8]) -> Vec<Export> {
    let wasm_names = parse_wasm_exports(wasm_bytes);
    let mut known: std::collections::HashSet<String> = std::collections::HashSet::new();
    for exp in &exports {
        known.insert(exp.js_name.clone());
        known.insert(exp.rust_name.clone());
    }
    let mut extras: Vec<Export> = Vec::new();
    for name in wasm_names {
        let rust_name = camel_to_snake(&name);
        if !known.contains(&name) && !known.contains(&rust_name) {
            known.insert(name.clone());
            known.insert(rust_name.clone());
            extras.push(Export {
                js_name: name,
                rust_name,
                args: Vec::new(),
                ret: ExportType::JsValue,
            });
        }
    }
    exports.extend(extras);
    exports
}

fn parse_jsdoc(lines: &[&str], mut i: usize) -> (DocSig, usize) {
    let mut args: Vec<ExportType> = Vec::new();
    let mut ret = ExportType::Void;

    while i < lines.len() {
        let doc_line = lines[i].trim();
        if doc_line == "*/" {
            return (DocSig { args, ret }, i + 1);
        }
        if let Some(ty) = parse_jsdoc_tag_type(doc_line, "@param") {
            args.push(ExportType::from_jsdoc(ty));
        }
        if let Some(ty) = parse_jsdoc_tag_type(doc_line, "@returns")
            .or_else(|| parse_jsdoc_tag_type(doc_line, "@return"))
        {
            ret = ExportType::from_jsdoc(ty);
        }
        i += 1;
    }

    (DocSig { args, ret }, i)
}

/// Parse `* @tag {Type}` lines and return the type string inside `{...}`.
fn parse_jsdoc_tag_type<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let after_star = line.strip_prefix('*')?.trim_start();
    let after_tag = after_star.strip_prefix(tag)?.trim_start();
    let start = after_tag.find('{')?;
    let rest = &after_tag[start + 1..];
    let end = rest.find('}')?;
    Some(rest[..end].trim())
}

/// Extract the local function name from `function foo(...)` declarations or
/// `const foo = function(...)` bindings.
fn extract_local_function_name(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("function ") {
        let open = rest.find('(')?;
        let name = rest[..open].trim();
        if is_valid_js_ident(name) {
            return Some(name.to_string());
        }
    }

    for prefix in ["const ", "let ", "var "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            let (name, rhs) = rest.split_once('=')?;
            let rhs = rhs.trim();
            if rhs.starts_with("function") {
                let name = name.trim();
                if is_valid_js_ident(name) {
                    return Some(name.to_string());
                }
            }
        }
    }

    None
}

fn parse_export_assignment(line: &str) -> Option<ExportAssign> {
    let rest = line.strip_prefix("exports.")?;
    let (name, rhs) = rest.split_once('=')?;
    let name = name.trim();
    if !is_valid_js_ident(name) || name.contains('.') {
        return None;
    }
    let value_expr = rhs.trim().trim_end_matches(';').trim().to_string();
    if value_expr.is_empty() {
        return None;
    }

    let value_ident = if is_valid_js_ident(&value_expr) {
        Some(value_expr.clone())
    } else {
        None
    };

    Some(ExportAssign {
        js_name: name.to_string(),
        function_param_count: function_expression_param_count(&value_expr),
        value_ident,
    })
}

fn build_export(
    assign: &ExportAssign,
    doc: Option<&DocSig>,
    typed_locals: &std::collections::HashMap<String, DocSig>,
) -> Export {
    let rust_name = camel_to_snake(&assign.js_name);
    let from_local = assign
        .value_ident
        .as_ref()
        .and_then(|ident| typed_locals.get(ident))
        .cloned();

    let (args, ret) = if let Some(local_sig) = from_local {
        (local_sig.args, local_sig.ret)
    } else if let Some(doc) = doc {
        (doc.args.clone(), doc.ret.clone())
    } else {
        (
            vec![ExportType::JsValue; assign.function_param_count.unwrap_or(0)],
            ExportType::JsValue,
        )
    };

    Export {
        js_name: assign.js_name.clone(),
        rust_name,
        args,
        ret,
    }
}

fn function_expression_param_count(expr: &str) -> Option<usize> {
    let rest = expr.strip_prefix("function")?.trim_start();
    let open = rest.find('(')?;
    let close = rest[open..].find(')')? + open;
    Some(count_params(&rest[open..=close]))
}

fn is_valid_js_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let valid_start = first == '_' || first == '$' || first.is_ascii_alphabetic();
    valid_start && chars.all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric())
}

/// Count parameters in a function signature line.
fn count_params(line: &str) -> usize {
    let Some(open) = line.find('(') else { return 0 };
    let Some(close) = line.rfind(')') else {
        return 0;
    };
    let params = &line[open + 1..close];
    if params.trim().is_empty() {
        0
    } else {
        params.split(',').count()
    }
}

/// Generate a Rust `extern "C"` block importing all `exports` from `npm_name`,
/// wrapped in a private `__wjb_peers` module to avoid polluting the call site's namespace.
///
/// The result is valid Rust source, suitable for writing to a tempfile and
/// including via the `wasm_peers!()` macro at compile time.
#[allow(dead_code)]
pub fn generate_peer_shim(exports: &[Export], npm_name: &str) -> String {
    generate_peer_shim_with_module(exports, npm_name, "__wjb_peers")
}

/// Same as [`generate_peer_shim`] but allows choosing a custom module name.
pub fn generate_peer_shim_with_module(
    exports: &[Export],
    npm_name: &str,
    module_name: &str,
) -> String {
    if exports.is_empty() {
        return String::new();
    }

    let mut inner = String::new();
    inner.push_str("    use wasm_bindgen::prelude::*;\n");
    inner.push_str(&format!("    #[wasm_bindgen(module = \"{npm_name}\")]\n"));
    inner.push_str("    extern \"C\" {\n");

    let mut has_exports = false;
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for exp in exports {
        // Skip internal wasm-bindgen glue exports.
        if exp.js_name.starts_with("__wbindgen") || exp.js_name.starts_with("__wbg_") {
            continue;
        }

        let rust_name = rust_ident(&exp.rust_name);
        if !seen_names.insert(rust_name.clone()) {
            continue;
        }

        has_exports = true;
        let args: Vec<String> = exp
            .args
            .iter()
            .enumerate()
            .map(|(i, ty)| format!("arg{i}: {}", ty.as_arg_type()))
            .collect();

        let ret = exp.ret.as_ret_type();
        let ret_str = if ret == "()" {
            String::new()
        } else {
            format!(" -> {ret}")
        };

        if exp.js_name != exp.rust_name || rust_name != exp.rust_name {
            inner.push_str(&format!(
                "        #[wasm_bindgen(js_name = \"{}\")]\n",
                exp.js_name
            ));
        }

        inner.push_str(&format!(
            "        pub fn {}({}){ret_str};\n",
            rust_name,
            args.join(", ")
        ));
    }

    if !has_exports {
        return String::new();
    }

    inner.push_str("    }\n");

    let mut out = String::new();
    out.push_str(&format!("mod {module_name} {{\n"));
    out.push_str(&inner);
    out.push_str("}\n");
    out.push_str(&format!("pub use {module_name}::*;\n"));
    out
}

fn rust_ident(name: &str) -> String {
    const RUST_KEYWORDS: &[&str] = &[
        "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
        "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
        "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
        "use", "where", "while", "async", "await", "dyn", "abstract", "become", "box", "do",
        "final", "macro", "override", "priv", "try", "typeof", "unsized", "virtual", "yield",
    ];

    if RUST_KEYWORDS.contains(&name) {
        format!("r#{name}")
    } else {
        name.to_string()
    }
}

/// Parse the WASM binary export section and return exported function names.
///
/// Reads section 7 (Export) of the WASM binary format. Skips non-function
/// exports and wasm-bindgen internal names (prefixed with `__`).
pub fn parse_wasm_exports(wasm_bytes: &[u8]) -> Vec<String> {
    const MAGIC: &[u8] = b"\0asm";
    const VERSION: &[u8] = &[1, 0, 0, 0];
    const SECTION_EXPORT: u8 = 7;
    const KIND_FUNCTION: u8 = 0;

    if wasm_bytes.len() < 8 {
        return Vec::new();
    }
    if &wasm_bytes[..4] != MAGIC || &wasm_bytes[4..8] != VERSION {
        return Vec::new();
    }

    let mut pos = 8;
    while pos < wasm_bytes.len() {
        let section_id = wasm_bytes[pos];
        pos += 1;
        if pos >= wasm_bytes.len() {
            break;
        }
        let (section_len, bytes_read) = leb128_u32(&wasm_bytes[pos..]);
        if bytes_read == 0 {
            break;
        }
        pos += bytes_read;
        let section_end = match pos.checked_add(section_len as usize) {
            Some(end) if end <= wasm_bytes.len() => end,
            _ => break,
        };

        if section_id == SECTION_EXPORT {
            let (count, n) = leb128_u32(&wasm_bytes[pos..]);
            if n == 0 {
                break;
            }
            let mut cur = pos + n;
            let mut names = Vec::new();
            for _ in 0..count {
                if cur >= section_end {
                    break;
                }
                let (name_len, n) = leb128_u32(&wasm_bytes[cur..]);
                if n == 0 {
                    break;
                }
                cur += n;
                let name_end = match cur.checked_add(name_len as usize) {
                    Some(end) if end <= section_end => end,
                    _ => break,
                };
                if name_end > section_end {
                    break;
                }
                let name = std::str::from_utf8(&wasm_bytes[cur..name_end])
                    .unwrap_or("")
                    .to_string();
                cur = name_end;
                if cur >= section_end {
                    break;
                }
                let kind = wasm_bytes[cur];
                cur += 1;
                let (_, n) = leb128_u32(&wasm_bytes[cur..]);
                if n == 0 {
                    break;
                }
                cur += n;
                if cur > section_end {
                    break;
                }
                if kind == KIND_FUNCTION && !name.starts_with("__") {
                    names.push(name);
                }
            }
            return names;
        }

        pos = section_end;
    }

    Vec::new()
}

/// Decode an unsigned LEB128 integer from `bytes`. Returns `(value, bytes_consumed)`.
fn leb128_u32(bytes: &[u8]) -> (u32, usize) {
    let mut result: u32 = 0;
    let mut shift = 0u32;
    let mut consumed = 0;
    for &byte in bytes {
        consumed += 1;
        result |= ((byte & 0x7f) as u32) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            break;
        }
        if shift >= 35 {
            break;
        }
    }
    (result, consumed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_JS: &str = r#"
'use strict';

/**
* @param {string} input
* @param {any} data
* @returns {boolean}
*/
exports.evalPredicate = function(input, data) {
    // ...
};

/**
* @param {string} selector
* @returns {string}
*/
exports.parseSelector = function(selector) {
    // ...
};

exports.__wbindgen_malloc = function() {};
"#;

    const SAMPLE_JS_DECL_THEN_EXPORT: &str = r#"
/**
 * @param {string} selector
 * @returns {boolean} whether selector is valid
 */
function validateSelector(selector) {
    return !!selector;
}
exports.validateSelector = validateSelector;
"#;

    #[test]
    fn parse_typed_exports() {
        // Arrange and Act
        let exports = parse_exports(SAMPLE_JS);

        // Assert
        let eval = exports
            .iter()
            .find(|e| e.js_name == "evalPredicate")
            .expect("evalPredicate should be parsed");
        assert_eq!(eval.rust_name, "eval_predicate", "camelCase → snake_case");
        assert_eq!(
            eval.args,
            vec![ExportType::Str, ExportType::JsValue],
            "arg types"
        );
        assert_eq!(eval.ret, ExportType::Bool, "return type");

        let parse = exports
            .iter()
            .find(|e| e.js_name == "parseSelector")
            .expect("parseSelector should be parsed");
        assert_eq!(parse.ret, ExportType::Str, "string return");
    }

    #[test]
    fn parse_typed_exports_from_function_decl_then_identity_export() {
        // Arrange and Act
        let exports = parse_exports(SAMPLE_JS_DECL_THEN_EXPORT);

        // Assert
        let validate = exports
            .iter()
            .find(|e| e.js_name == "validateSelector")
            .expect("validateSelector should be parsed");
        assert_eq!(
            validate.args,
            vec![ExportType::Str],
            "param should be typed from JSDoc"
        );
        assert_eq!(
            validate.ret,
            ExportType::Bool,
            "return type should parse even with description text"
        );
    }

    #[test]
    fn parse_exports_empty_when_no_exports_present() {
        // Arrange and Act
        let exports = parse_exports("function helper() {}\nconst x = 1;\n");

        // Assert
        assert!(
            exports.is_empty(),
            "should return empty for files with no exports.* assignments"
        );
    }

    #[test]
    fn skips_wbindgen_internals() {
        // Arrange and Act
        let exports = parse_exports(SAMPLE_JS);
        let shim = generate_peer_shim(&exports, "@aql/engine");

        // Assert
        assert!(
            !shim.contains("__wbindgen"),
            "internal glue should be excluded"
        );
    }

    #[test]
    fn generate_shim_types() {
        // Arrange
        let exports = vec![Export {
            js_name: "evalPredicate".into(),
            rust_name: "eval_predicate".into(),
            args: vec![ExportType::Str, ExportType::JsValue],
            ret: ExportType::Bool,
        }];

        // Act
        let shim = generate_peer_shim(&exports, "@aql/engine");

        // Assert
        assert!(shim.contains("module = \"@aql/engine\""), "module attr");
        assert!(
            shim.contains("pub fn eval_predicate(arg0: &str, arg1: JsValue) -> bool"),
            "signature"
        );
        assert!(shim.contains("js_name = \"evalPredicate\""), "js_name attr");
    }

    #[test]
    fn generate_shim_uses_custom_module_and_raw_ident() {
        // Arrange
        let exports = vec![Export {
            js_name: "type".into(),
            rust_name: "type".into(),
            args: vec![],
            ret: ExportType::Void,
        }];

        // Act
        let shim = generate_peer_shim_with_module(&exports, "@aql/engine", "__wjb_peers_dep0");

        // Assert
        assert!(
            shim.contains("mod __wjb_peers_dep0"),
            "custom module name should be used"
        );
        assert!(
            shim.contains("pub fn r#type()"),
            "Rust keyword should be emitted as raw ident"
        );
    }
}
