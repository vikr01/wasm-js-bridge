//! Flow type declaration generation from Rust descriptors.

use crate::types::{Interface, TypeAlias, WasmFn};

/// An opaque type declaration for Flow.
pub struct OpaqueType {
    /// Type name (e.g. `"Manifest"`, `"TagName"`).
    pub name: &'static str,
    /// Optional supertype bound. `Some("string")` → `opaque type TagName: string`.
    /// `None` → fully opaque (`opaque type Manifest`).
    pub bound: Option<&'static str>,
}

/// Generate `index.js.flow` content from type declarations and function descriptors.
///
/// `flow_decls` are pre-rendered strings from `flowjs_rs::Flow::decl()` (native Flow syntax).
/// Types listed in `opaque_types` replace their declaration with an opaque declaration.
pub fn generate_index_flow(
    flow_decls: &[String],
    aliases: &[TypeAlias],
    interfaces: &[Interface],
    fns: &[WasmFn],
    opaque_types: &[OpaqueType],
) -> String {
    let mut out = String::from("// @flow\n// Generated from Rust via flowjs-rs. Do not edit.\n\n");

    let opaque_names: Vec<&str> = opaque_types.iter().map(|o| o.name).collect();

    // Opaque type declarations first
    for opaque in opaque_types {
        match opaque.bound {
            Some(bound) => {
                out.push_str(&format!(
                    "declare export opaque type {}: {bound};\n\n",
                    opaque.name
                ));
            }
            None => {
                out.push_str(&format!("declare export opaque type {};\n\n", opaque.name));
            }
        }
    }

    // Non-opaque type declarations (already native Flow syntax)
    for decl in flow_decls {
        let name = extract_type_name(decl);
        if opaque_names.contains(&name) {
            continue;
        }
        if decl.trim_start().starts_with("declare ") {
            out.push_str(&format!("{decl}\n\n"));
        } else {
            out.push_str(&format!("export {decl}\n\n"));
        }
    }

    for alias in aliases {
        if opaque_names.contains(&alias.name) {
            continue;
        }
        out.push_str(&format!("export type {} = {};\n", alias.name, alias.target));
    }
    if !aliases.is_empty() {
        out.push('\n');
    }

    for iface in interfaces {
        out.push_str(&format!("export interface {} {{\n", iface.name));
        for (name, ty) in iface.fields {
            out.push_str(&format!("  +{name}: {ty},\n"));
        }
        out.push_str("}\n\n");
    }

    for f in fns {
        out.push_str(&format!(
            "declare export function {}({}): {};\n",
            f.name,
            (f.flow_params)(),
            (f.flow_ret)()
        ));
    }

    out
}

/// Extract the type name from a Flow declaration string.
///
/// Tries all common Flow declaration prefixes (longest first to avoid partial matches).
fn extract_type_name(decl: &str) -> &str {
    let s = decl.trim();
    for prefix in [
        "declare export opaque type ",
        "declare export type ",
        "declare opaque type ",
        "declare type ",
        "opaque type ",
        "declare export interface ",
        "declare interface ",
        "export interface ",
        "interface ",
        "export type ",
        "type ",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return rest.split([' ', '<', '=', ':']).next().unwrap_or("");
        }
    }
    ""
}


#[cfg(test)]
mod tests {
    use super::*;

    fn p_input_string() -> String {
        "input: string".to_string()
    }
    fn r_predicate() -> String {
        "Predicate".to_string()
    }
    fn p_files_ro_array() -> String {
        "files: $ReadOnlyArray<FileEntry>".to_string()
    }
    fn r_annotations_ro_array() -> String {
        "$ReadOnlyArray<Annotation>".to_string()
    }
    fn p_file_nullable() -> String {
        "file?: ?RelativePath".to_string()
    }
    fn r_string() -> String {
        "string".to_string()
    }

    fn make_fn(
        name: &'static str,
        flow_params: fn() -> String,
        flow_ret: fn() -> String,
    ) -> WasmFn {
        WasmFn {
            name,
            file: "src/lib.rs",
            ts_params: flow_params,
            ts_ret: flow_ret,
            flow_params,
            flow_ret,
        }
    }

    #[test]
    fn generates_opaque_type_with_bound() {
        // Arrange
        let opaque = &[OpaqueType {
            name: "TagName",
            bound: Some("string"),
        }];

        // Act
        let flow = generate_index_flow(&[], &[], &[], &[], opaque);

        // Assert
        assert!(
            flow.contains("declare export opaque type TagName: string;"),
            "should emit bounded opaque type"
        );
    }

    #[test]
    fn generates_fully_opaque_type() {
        // Arrange
        let opaque = &[OpaqueType {
            name: "Manifest",
            bound: None,
        }];

        // Act
        let flow = generate_index_flow(&[], &[], &[], &[], opaque);

        // Assert
        assert!(
            flow.contains("declare export opaque type Manifest;"),
            "should emit fully opaque type"
        );
    }

    #[test]
    fn skips_opaque_type_from_decls() {
        // Arrange
        let flow_decls = vec!["type TagName = string;".to_string()];
        let opaque = &[OpaqueType {
            name: "TagName",
            bound: Some("string"),
        }];

        // Act
        let flow = generate_index_flow(&flow_decls, &[], &[], &[], opaque);

        // Assert
        assert!(
            !flow.contains("export type TagName = string;"),
            "should not emit flow decl for opaque type"
        );
        assert!(
            flow.contains("declare export opaque type TagName: string;"),
            "should emit opaque declaration instead"
        );
    }

    #[test]
    fn emits_native_flow_decl_directly() {
        // Arrange
        let flow_decls = vec!["type Foo = {| +bar: string, +baz: number |};".to_string()];

        // Act
        let flow = generate_index_flow(&flow_decls, &[], &[], &[], &[]);

        // Assert
        assert!(
            flow.contains("export type Foo = {| +bar: string, +baz: number |};"),
            "should emit native flow decl without conversion"
        );
    }

    #[test]
    fn uses_flow_params_fn_pointer() {
        // Arrange — fn pointers already produce Flow-native strings
        let fns = &[make_fn("select", p_files_ro_array, r_annotations_ro_array)];

        // Act
        let flow = generate_index_flow(&[], &[], &[], fns, &[]);

        // Assert
        assert!(
            flow.contains("$ReadOnlyArray<Annotation>"),
            "should use flow_ret fn pointer result"
        );
        assert!(
            flow.contains("$ReadOnlyArray<FileEntry>"),
            "should use flow_params fn pointer result"
        );
    }

    #[test]
    fn uses_nullable_flow_param_fn_pointer() {
        // Arrange — fn pointer already produces Flow ?Type notation
        let fns = &[make_fn("select", p_file_nullable, r_string)];

        // Act
        let flow = generate_index_flow(&[], &[], &[], fns, &[]);

        // Assert
        assert!(
            flow.contains("file?: ?RelativePath"),
            "should use flow_params fn pointer with ?T notation"
        );
    }

    #[test]
    fn generates_flow_interface_with_covariant_fields() {
        // Arrange
        let interfaces = &[Interface {
            name: "RemoveResult",
            fields: &[("result", "MutationResult"), ("detached", "string")],
        }];

        // Act
        let flow = generate_index_flow(&[], &[], interfaces, &[], &[]);

        // Assert
        assert!(
            flow.contains("+result: MutationResult,"),
            "should use + for readonly fields"
        );
        assert!(
            flow.contains("+detached: string,"),
            "should use + for readonly fields"
        );
    }

    #[test]
    fn generates_declare_export_function() {
        // Arrange
        let fns = &[make_fn("parsePredicate", p_input_string, r_predicate)];

        // Act
        let flow = generate_index_flow(&[], &[], &[], fns, &[]);

        // Assert
        assert!(
            flow.contains("declare export function parsePredicate(input: string): Predicate;"),
            "should use declare export function"
        );
    }

    #[test]
    fn has_flow_pragma() {
        // Act
        let flow = generate_index_flow(&[], &[], &[], &[], &[]);

        // Assert
        assert!(
            flow.starts_with("// @flow\n"),
            "should start with @flow pragma"
        );
    }

    #[test]
    fn extract_type_name_handles_declare_export() {
        // Arrange and Act and Assert
        assert_eq!(
            extract_type_name("declare export type Foo = string;"),
            "Foo",
            "declare export type"
        );
        assert_eq!(
            extract_type_name("declare export opaque type TagName: string;"),
            "TagName",
            "declare export opaque type"
        );
        assert_eq!(
            extract_type_name("type Bar = number;"),
            "Bar",
            "plain type"
        );
    }
}
