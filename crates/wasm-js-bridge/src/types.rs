//! Shared descriptor types for code generation targets.

/// A WASM-exported function descriptor.
#[derive(Clone, Copy)]
pub struct WasmFn {
    /// camelCase JS function name (wasm-bindgen auto-converts snake_case).
    pub name: &'static str,
    /// Raw output of `file!()` at `#[wasm_export]` call site (e.g. `"src/foo_bar.rs"`).
    /// Used by `bundle!` to group functions by source file → output stem.
    pub file: &'static str,
    /// Called at test runtime — returns TS parameter signature string.
    pub ts_params: fn() -> String,
    /// Called at test runtime — returns TS return type string.
    pub ts_ret: fn() -> String,
    /// Called at test runtime — returns Flow parameter signature string.
    pub flow_params: fn() -> String,
    /// Called at test runtime — returns Flow return type string.
    pub flow_ret: fn() -> String,
}

/// An interface not modeled as a Rust struct (e.g. ad-hoc WASM return shapes).
pub struct Interface {
    /// Interface name.
    pub name: &'static str,
    /// Fields: `(name, type)`. All emitted as readonly/covariant.
    pub fields: &'static [(&'static str, &'static str)],
}

/// A type alias (e.g. `export type AttrOp = PredicateOp`).
pub struct TypeAlias {
    /// Alias name.
    pub name: &'static str,
    /// Target type.
    pub target: &'static str,
}
