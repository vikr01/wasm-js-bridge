//! TypeScript declaration generation from Rust descriptors.

use crate::types::{Interface, TypeAlias, WasmFn};

/// Generate `{stem}.d.ts` content from type declarations and function descriptors.
///
/// `type_decls` are pre-rendered strings from `ts_rs::TS::decl()`.
pub fn generate_index_dts(
    type_decls: &[String],
    aliases: &[TypeAlias],
    interfaces: &[Interface],
    fns: &[WasmFn],
) -> String {
    let mut out = String::from("// Generated from Rust via ts-rs. Do not edit.\n\n");

    for decl in type_decls {
        out.push_str(&format!("export {decl}\n\n"));
    }

    for alias in aliases {
        out.push_str(&format!("export type {} = {};\n", alias.name, alias.target));
    }
    if !aliases.is_empty() {
        out.push('\n');
    }

    for iface in interfaces {
        out.push_str(&format!("export interface {} {{\n", iface.name));
        for (name, ty) in iface.fields {
            out.push_str(&format!("  readonly {name}: {ty};\n"));
        }
        out.push_str("}\n\n");
    }

    for f in fns {
        out.push_str(&format!(
            "export function {}({}): {};\n",
            f.name,
            (f.ts_params)(),
            (f.ts_ret)()
        ));
    }

    out
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
    fn p_files_readonly() -> String {
        "files: ReadonlyArray<FileEntry>".to_string()
    }
    fn r_annotations_readonly() -> String {
        "ReadonlyArray<Annotation>".to_string()
    }

    fn make_fn(
        name: &'static str,
        ts_params: fn() -> String,
        ts_ret: fn() -> String,
    ) -> WasmFn {
        WasmFn {
            name,
            file: "src/lib.rs",
            ts_params,
            ts_ret,
            flow_params: ts_params,
            flow_ret: ts_ret,
        }
    }

    #[test]
    fn generates_dts_with_types_and_fns() {
        // Arrange
        let type_decls = vec!["type Predicate = { name: string }".to_string()];
        let fns = &[make_fn("parsePredicate", p_input_string, r_predicate)];

        // Act
        let dts = generate_index_dts(&type_decls, &[], &[], fns);

        // Assert
        assert!(
            dts.contains("export type Predicate = { name: string }"),
            "should include type declaration"
        );
        assert!(
            dts.contains("export function parsePredicate(input: string): Predicate;"),
            "should include function signature"
        );
    }

    #[test]
    fn generates_interfaces() {
        // Arrange
        let interfaces = &[Interface {
            name: "RemoveResult",
            fields: &[("result", "MutationResult"), ("detached", "string")],
        }];

        // Act
        let dts = generate_index_dts(&[], &[], interfaces, &[]);

        // Assert
        assert!(
            dts.contains("export interface RemoveResult"),
            "should have interface"
        );
        assert!(
            dts.contains("readonly result: MutationResult;"),
            "should have readonly field"
        );
    }

    #[test]
    fn generates_type_aliases() {
        // Arrange
        let aliases = &[TypeAlias {
            name: "AttrOp",
            target: "PredicateOp",
        }];

        // Act
        let dts = generate_index_dts(&[], aliases, &[], &[]);

        // Assert
        assert!(
            dts.contains("export type AttrOp = PredicateOp;"),
            "should have type alias"
        );
    }

    #[test]
    fn generates_fn_with_fn_pointer_types() {
        // Arrange — fn pointers compute type strings at call time
        let fns = &[make_fn("select", p_files_readonly, r_annotations_readonly)];

        // Act
        let dts = generate_index_dts(&[], &[], &[], fns);

        // Assert
        assert!(
            dts.contains(
                "export function select(files: ReadonlyArray<FileEntry>): ReadonlyArray<Annotation>;"
            ),
            "should use fn pointer results for type strings"
        );
    }
}
