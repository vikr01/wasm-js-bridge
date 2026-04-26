//! Proc macros for wasm-js-bridge code generation.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    parse_macro_input, FnArg, GenericArgument, ItemFn, LitStr, Pat, PathArguments, ReturnType,
    Token, Type,
};

// ---------------------------------------------------------------------------
// Type classification helpers
// ---------------------------------------------------------------------------

fn is_str_ref(ty: &Type) -> bool {
    matches!(ty, Type::Reference(r) if matches!(r.elem.as_ref(), Type::Path(p) if p.path.is_ident("str")))
}

fn is_string(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.is_ident("String"))
}

fn is_bool(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.is_ident("bool"))
}

fn is_numeric(ty: &Type) -> bool {
    if let Type::Path(p) = ty {
        if let Some(ident) = p.path.get_ident() {
            return matches!(
                ident.to_string().as_str(),
                "u8" | "u16"
                    | "u32"
                    | "u64"
                    | "i8"
                    | "i16"
                    | "i32"
                    | "i64"
                    | "f32"
                    | "f64"
                    | "usize"
                    | "isize"
            );
        }
    }
    false
}

fn unwrap_single_generic<'a>(ty: &'a Type, name: &str) -> Option<&'a Type> {
    if let Type::Path(p) = ty {
        let last = p.path.segments.last()?;
        if last.ident != name {
            return None;
        }
        if let PathArguments::AngleBracketed(args) = &last.arguments {
            if let Some(GenericArgument::Type(inner)) = args.args.first() {
                return Some(inner);
            }
        }
    }
    None
}

fn unwrap_option(ty: &Type) -> Option<&Type> {
    unwrap_single_generic(ty, "Option")
}

fn unwrap_vec(ty: &Type) -> Option<&Type> {
    unwrap_single_generic(ty, "Vec")
}

fn unwrap_result(ty: &Type) -> Option<(&Type, &Type)> {
    if let Type::Path(p) = ty {
        let last = p.path.segments.last()?;
        if last.ident != "Result" {
            return None;
        }
        if let PathArguments::AngleBracketed(args) = &last.arguments {
            let mut iter = args.args.iter();
            if let (Some(GenericArgument::Type(ok)), Some(GenericArgument::Type(err))) =
                (iter.next(), iter.next())
            {
                return Some((ok, err));
            }
        }
    }
    None
}

fn contains_nested_reference(ty: &Type) -> bool {
    match ty {
        Type::Reference(_) => true,
        Type::Array(a) => contains_nested_reference(&a.elem),
        Type::Group(g) => contains_nested_reference(&g.elem),
        Type::Paren(p) => contains_nested_reference(&p.elem),
        Type::Slice(s) => contains_nested_reference(&s.elem),
        Type::Tuple(t) => t.elems.iter().any(contains_nested_reference),
        Type::Path(p) => p.path.segments.iter().any(|seg| {
            if let PathArguments::AngleBracketed(args) = &seg.arguments {
                args.args.iter().any(|arg| match arg {
                    GenericArgument::Type(inner) => contains_nested_reference(inner),
                    _ => false,
                })
            } else {
                false
            }
        }),
        _ => false,
    }
}

/// Strip any `r#` raw-identifier prefix from a param ident before emitting as JS.
fn js_param_name(ident: &syn::Ident) -> String {
    let s = ident.to_string();
    s.strip_prefix("r#").unwrap_or(&s).to_string()
}

// ---------------------------------------------------------------------------
// WASM adapter parameter generation
// ---------------------------------------------------------------------------

/// Returns (wasm_param_decl, deserialization_stmt).
/// `wasm_param_decl` is used in the adapter fn signature.
/// `deserialization_stmt` rebinds the param to the Rust type (empty if no conversion needed).
fn wasm_param(name: &syn::Ident, ty: &Type) -> (TokenStream2, TokenStream2) {
    if is_str_ref(ty) {
        // &str -> pass directly (wasm-bindgen native support)
        (quote!(#name: &str), quote!())
    } else if let Type::Reference(r) = ty {
        if r.mutability.is_some() {
            return (
                syn::Error::new_spanned(
                    ty,
                    "#[wasm_export] does not support &mut T params; take T by value",
                )
                .to_compile_error(),
                quote!(),
            );
        }
        if matches!(r.elem.as_ref(), Type::Slice(_)) {
            return (
                syn::Error::new_spanned(
                    ty,
                    "#[wasm_export] does not support &[T] params; use Vec<T>",
                )
                .to_compile_error(),
                quote!(),
            );
        }
        // &T (non-str, non-mut, non-slice) -> receive JsValue, deserialize as T, rebind as &T
        let inner = &*r.elem;
        let owned = format_ident!("{name}_owned_");
        (
            quote!(#name: ::wasm_bindgen::JsValue),
            quote!(
                let #owned: #inner = ::serde_wasm_bindgen::from_value(#name)
                    .map_err(|e| ::wasm_bindgen::JsError::new(&e.to_string()))?;
                let #name = &#owned;
            ),
        )
    } else if is_string(ty) {
        (quote!(#name: String), quote!())
    } else if is_bool(ty) {
        (quote!(#name: bool), quote!())
    } else if is_numeric(ty) {
        (quote!(#name: #ty), quote!())
    } else if contains_nested_reference(ty) {
        (
            syn::Error::new_spanned(
                ty,
                "#[wasm_export] does not support borrowed references inside generic/container types (e.g. Option<&T>, Vec<&T>); use owned data",
            )
            .to_compile_error(),
            quote!(),
        )
    } else {
        (
            quote!(#name: ::wasm_bindgen::JsValue),
            quote!(
                let #name: #ty = ::serde_wasm_bindgen::from_value(#name)
                    .map_err(|e| ::wasm_bindgen::JsError::new(&e.to_string()))?;
            ),
        )
    }
}

fn wasm_return_body(ret_ty: &Type, call: TokenStream2) -> TokenStream2 {
    if let Some((_, err_ty)) = unwrap_result(ret_ty) {
        let err_conv = if is_string(err_ty) {
            quote!(|e| ::wasm_bindgen::JsError::new(&e))
        } else {
            quote!(|e| ::wasm_bindgen::JsError::new(&e.to_string()))
        };
        quote!(
            let __result = #call.map_err(#err_conv)?;
            let __serializer = ::serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
            ::serde::Serialize::serialize(&__result, &__serializer)
                .map_err(|e| ::wasm_bindgen::JsError::new(&e.to_string()))
        )
    } else {
        quote!(
            let __result = #call;
            let __serializer = ::serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
            ::serde::Serialize::serialize(&__result, &__serializer)
                .map_err(|e| ::wasm_bindgen::JsError::new(&e.to_string()))
        )
    }
}

// ---------------------------------------------------------------------------
// Type string code generation (for WasmFn descriptor helper fns)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Dialect {
    Ts,
    Flow,
}

/// Generate code that, when executed, produces a type string for the given Rust type.
///
/// References are transparent at the JS boundary. Vec<T> maps to the dialect's readonly
/// array form. Option<T> maps to nullable. Result<T, _> maps to T (errors surface as
/// thrown exceptions). All other types delegate to the dialect's trait (`TS` or `Flow`).
fn type_expr(ty: &Type, dialect: Dialect) -> TokenStream2 {
    if is_str_ref(ty) || is_string(ty) {
        quote!("string".to_string())
    } else if let Type::Reference(r) = ty {
        type_expr(&r.elem, dialect)
    } else if is_bool(ty) {
        quote!("boolean".to_string())
    } else if let Some(inner) = unwrap_vec(ty) {
        let inner_expr = type_expr(inner, dialect);
        match dialect {
            Dialect::Ts => quote!(format!("ReadonlyArray<{}>", #inner_expr)),
            Dialect::Flow => quote!(format!("$ReadOnlyArray<{}>", #inner_expr)),
        }
    } else if let Some(inner) = unwrap_option(ty) {
        let inner_expr = type_expr(inner, dialect);
        match dialect {
            Dialect::Ts => quote!(format!("{} | null", #inner_expr)),
            Dialect::Flow => quote!(format!("?{}", #inner_expr)),
        }
    } else if let Some((ok_ty, _)) = unwrap_result(ty) {
        type_expr(ok_ty, dialect)
    } else {
        match dialect {
            Dialect::Ts => quote!(<#ty as ::ts_rs::TS>::name(&cfg)),
            Dialect::Flow => quote!(<#ty as ::flowjs_rs::Flow>::name(&cfg)),
        }
    }
}

/// Strip a leading reference to get the underlying type for optionality detection.
fn deref_ty(ty: &Type) -> &Type {
    if let Type::Reference(r) = ty {
        &r.elem
    } else {
        ty
    }
}

fn params_body(params: &[(syn::Ident, Type)], dialect: Dialect) -> TokenStream2 {
    if params.is_empty() {
        return quote!(String::new());
    }
    let parts: Vec<TokenStream2> = params
        .iter()
        .enumerate()
        .map(|(i, (name, ty))| {
            let name_str = js_param_name(name);
            if let Some(inner) = unwrap_option(deref_ty(ty)) {
                let inner_expr = type_expr(inner, dialect);
                match dialect {
                    Dialect::Ts => {
                        // Use ?: only when all remaining params are also Option<T>
                        let all_remaining_optional = params[i..]
                            .iter()
                            .all(|(_, t)| unwrap_option(deref_ty(t)).is_some());
                        if all_remaining_optional {
                            quote!(format!("{}?: {} | null", #name_str, #inner_expr))
                        } else {
                            quote!(format!("{}: {} | null | undefined", #name_str, #inner_expr))
                        }
                    }
                    Dialect::Flow => quote!(format!("{}: ?{}", #name_str, #inner_expr)),
                }
            } else {
                let expr = type_expr(ty, dialect);
                quote!(format!("{}: {}", #name_str, #expr))
            }
        })
        .collect();
    quote!([#(#parts),*].join(", "))
}

// ---------------------------------------------------------------------------
// Helpers
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

fn snake_to_screaming(s: &str) -> String {
    s.to_uppercase()
}

// ---------------------------------------------------------------------------
// #[wasm_export] -- reads Rust function signature, no string annotations
// ---------------------------------------------------------------------------

/// Mark a pure Rust function as a wasm-js-bridge WASM export.
///
/// Emits three things:
/// 1. The original function, unchanged -- works for any Rust consumer.
/// 2. A `#[wasm_bindgen]` adapter under `#[cfg(feature = "wasm")]` that
///    deserializes complex params via `serde_wasm_bindgen` and serializes output.
/// 3. A `WasmFn` const descriptor + helper fns under
///    `#[cfg(all(feature = "codegen", any(feature = "ts", feature = "flow")))]`
///    for npm package codegen (used by `bundle!`).
///
/// # Example
///
/// ```ignore
/// #[wasm_export]
/// pub fn parse_selector(selector: &str) -> Result<SelectorAst, MyError> { ... }
/// ```
#[proc_macro_attribute]
pub fn wasm_export(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[wasm_export] takes no arguments -- it reads the Rust function signature directly",
        )
        .to_compile_error()
        .into();
    }

    let func = parse_macro_input!(item as ItemFn);

    if func.sig.asyncness.is_some() {
        return syn::Error::new_spanned(
            &func.sig,
            "#[wasm_export] does not support async functions",
        )
        .to_compile_error()
        .into();
    }
    if !func.sig.generics.params.is_empty() {
        return syn::Error::new_spanned(
            &func.sig.generics,
            "#[wasm_export] does not support generic functions",
        )
        .to_compile_error()
        .into();
    }

    let fn_name = &func.sig.ident;
    let fn_name_str = fn_name.to_string();
    let fn_name_bare = fn_name_str.strip_prefix("r#").unwrap_or(&fn_name_str);
    let js_name = snake_to_camel(fn_name_bare);
    let wasm_fn_name = format_ident!("__wasm_{}", fn_name);
    let const_name = format_ident!(
        "_WASM_JS_BRIDGE_{}",
        snake_to_screaming(&fn_name.to_string())
    );
    let ts_params_fn = format_ident!("__wjb_ts_params_{}", fn_name);
    let ts_ret_fn = format_ident!("__wjb_ts_ret_{}", fn_name);
    let flow_params_fn = format_ident!("__wjb_flow_params_{}", fn_name);
    let flow_ret_fn = format_ident!("__wjb_flow_ret_{}", fn_name);

    // Collect typed parameters -- error on self receivers or destructuring patterns
    let mut typed_params: Vec<(syn::Ident, Type)> = Vec::new();
    for arg in &func.sig.inputs {
        match arg {
            FnArg::Receiver(r) => {
                return syn::Error::new_spanned(
                    r,
                    "#[wasm_export] does not support `self` receivers",
                )
                .to_compile_error()
                .into();
            }
            FnArg::Typed(pt) => match pt.pat.as_ref() {
                Pat::Ident(pi) => typed_params.push((pi.ident.clone(), *pt.ty.clone())),
                _ => {
                    return syn::Error::new_spanned(
                        &pt.pat,
                        "#[wasm_export] requires simple identifier patterns; destructuring and `_` are not supported",
                    )
                    .to_compile_error()
                    .into();
                }
            },
        }
    }

    // Return type
    let ret_ty: Type = match &func.sig.output {
        ReturnType::Default => syn::parse_quote!(()),
        ReturnType::Type(_, ty) => *ty.clone(),
    };

    // WASM adapter params
    let (wasm_param_decls, wasm_deser_stmts): (Vec<_>, Vec<_>) = typed_params
        .iter()
        .map(|(name, ty)| wasm_param(name, ty))
        .unzip();

    // Build call expression
    let param_idents: Vec<&syn::Ident> = typed_params.iter().map(|(n, _)| n).collect();
    let call_expr = quote!(#fn_name(#(#param_idents),*));

    // WASM adapter return body
    let wasm_body = wasm_return_body(&ret_ty, call_expr);

    // TS/Flow type expressions
    let ts_params_expr = params_body(&typed_params, Dialect::Ts);
    let ts_ret_expr = type_expr(&ret_ty, Dialect::Ts);
    let flow_params_expr = params_body(&typed_params, Dialect::Flow);
    let flow_ret_expr = type_expr(&ret_ty, Dialect::Flow);

    // Split attrs: non-doc attrs (e.g. #[cfg(...)], #[allow(...)]) are propagated
    // to the WASM adapter and WasmFn descriptor so conditional compilation is
    // preserved. Doc attrs stay only on the original fn (already included via
    // `all_attrs`).
    // Propagate cfg/allow/deny/warn but not doc or fn-only attrs like must_use
    // (which is invalid on consts and helper fns).
    let non_doc_attrs: Vec<_> = func
        .attrs
        .iter()
        .filter(|a| {
            let path = a.path();
            !path.is_ident("doc") && !path.is_ident("must_use")
        })
        .collect();
    let all_attrs = &func.attrs;

    let vis = &func.vis;
    let sig = &func.sig;
    let block = &func.block;

    // The WasmFn descriptor is gated on codegen + at least one declaration target.
    // Each helper fn has a real implementation for its feature and a fallback
    // implementation otherwise, so descriptor emission works with ts-only or flow-only.
    let descriptor_cfg = quote!(all(
        feature = "codegen",
        any(feature = "ts", feature = "flow")
    ));

    quote! {
        // 1. Original function -- unchanged, no WASM overhead for Rust consumers
        #(#all_attrs)*
        #vis #sig #block

        // 2. WASM adapter -- only compiled when wasm feature is enabled.
        //    Non-doc attrs (e.g. #[cfg(...)]) are propagated so the adapter
        //    inherits the same conditional compilation as the original fn.
        #(#non_doc_attrs)*
        #[cfg(feature = "wasm")]
        #[::wasm_bindgen::prelude::wasm_bindgen(js_name = #js_name)]
        pub fn #wasm_fn_name(
            #(#wasm_param_decls),*
        ) -> ::std::result::Result<::wasm_bindgen::JsValue, ::wasm_bindgen::JsError> {
            #(#wasm_deser_stmts)*
            #wasm_body
        }

        // 3. WasmFn descriptor -- compiled when codegen+ts+flow features are enabled.
        //    Non-doc attrs are propagated so cfg-gated fns don't appear in wrong builds.
        #(#non_doc_attrs)*
        #[cfg(all(feature = "codegen", feature = "ts"))]
        fn #ts_params_fn() -> String {
            let cfg: ::ts_rs::Config = ::std::default::Default::default();
            #ts_params_expr
        }
        #(#non_doc_attrs)*
        #[cfg(all(feature = "codegen", not(feature = "ts")))]
        fn #ts_params_fn() -> String {
            "any".to_string()
        }
        #(#non_doc_attrs)*
        #[cfg(all(feature = "codegen", feature = "ts"))]
        fn #ts_ret_fn() -> String {
            let cfg: ::ts_rs::Config = ::std::default::Default::default();
            #ts_ret_expr
        }
        #(#non_doc_attrs)*
        #[cfg(all(feature = "codegen", not(feature = "ts")))]
        fn #ts_ret_fn() -> String {
            "any".to_string()
        }
        #(#non_doc_attrs)*
        #[cfg(all(feature = "codegen", feature = "flow"))]
        fn #flow_params_fn() -> String {
            let cfg: ::flowjs_rs::Config = ::std::default::Default::default();
            #flow_params_expr
        }
        #(#non_doc_attrs)*
        #[cfg(all(feature = "codegen", not(feature = "flow")))]
        fn #flow_params_fn() -> String {
            "any".to_string()
        }
        #(#non_doc_attrs)*
        #[cfg(all(feature = "codegen", feature = "flow"))]
        fn #flow_ret_fn() -> String {
            let cfg: ::flowjs_rs::Config = ::std::default::Default::default();
            #flow_ret_expr
        }
        #(#non_doc_attrs)*
        #[cfg(all(feature = "codegen", not(feature = "flow")))]
        fn #flow_ret_fn() -> String {
            "any".to_string()
        }
        #(#non_doc_attrs)*
        #[cfg(#descriptor_cfg)]
        #[doc(hidden)]
        #[allow(dead_code)]
        #vis const #const_name: ::wasm_js_bridge::WasmFn = ::wasm_js_bridge::WasmFn {
            name: #js_name,
            file: file!(),
            ts_params: #ts_params_fn,
            ts_ret: #ts_ret_fn,
            flow_params: #flow_params_fn,
            flow_ret: #flow_ret_fn,
        };
    }
    .into()
}

// ---------------------------------------------------------------------------
// bundle! -- replaces hand-written ts_codegen test modules
// ---------------------------------------------------------------------------

struct BundleArgs {
    types: Vec<syn::Type>,
    fns: Vec<syn::Ident>,
    aliases: Vec<(String, String)>,
    opaque: Vec<(String, Option<String>)>,
}

impl syn::parse::Parse for BundleArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut types = Vec::new();
        let mut fns = Vec::new();
        let mut aliases = Vec::new();
        let mut opaque = Vec::new();

        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;

            match key.to_string().as_str() {
                "types" => {
                    let content;
                    syn::bracketed!(content in input);
                    while !content.is_empty() {
                        types.push(content.parse::<syn::Type>()?);
                        if !content.is_empty() {
                            content.parse::<Token![,]>()?;
                        }
                    }
                }
                "fns" => {
                    let content;
                    syn::bracketed!(content in input);
                    while !content.is_empty() {
                        fns.push(content.parse::<syn::Ident>()?);
                        if !content.is_empty() {
                            content.parse::<Token![,]>()?;
                        }
                    }
                }
                "aliases" => {
                    let content;
                    syn::bracketed!(content in input);
                    while !content.is_empty() {
                        let tuple;
                        syn::parenthesized!(tuple in content);
                        let name: LitStr = tuple.parse()?;
                        tuple.parse::<Token![,]>()?;
                        let target: LitStr = tuple.parse()?;
                        aliases.push((name.value(), target.value()));
                        if !content.is_empty() {
                            content.parse::<Token![,]>()?;
                        }
                    }
                }
                "opaque" => {
                    let content;
                    syn::bracketed!(content in input);
                    while !content.is_empty() {
                        let tuple;
                        syn::parenthesized!(tuple in content);
                        let name: LitStr = tuple.parse()?;
                        tuple.parse::<Token![,]>()?;
                        // Parse `None` or `Some("bound")`
                        let ident: syn::Ident = tuple.parse()?;
                        let bound = if ident == "None" {
                            None
                        } else if ident == "Some" {
                            let inner;
                            syn::parenthesized!(inner in tuple);
                            let lit: LitStr = inner.parse()?;
                            Some(lit.value())
                        } else {
                            return Err(syn::Error::new(
                                ident.span(),
                                "expected `None` or `Some(\"bound\")`",
                            ));
                        };
                        opaque.push((name.value(), bound));
                        if !content.is_empty() {
                            content.parse::<Token![,]>()?;
                        }
                    }
                }
                _ => {
                    return Err(syn::Error::new(key.span(), format!("unknown key: {key}")));
                }
            }

            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(BundleArgs {
            types,
            fns,
            aliases,
            opaque,
        })
    }
}

/// Generate `#[test] fn generate_npm_files()` that writes `.d.ts` and/or
/// `.js.flow` output files depending on enabled features.
///
/// Groups functions by source file stem and writes one output file set per stem.
/// `"src/lib.rs"` -> `"lib"`, `"src/foo_bar.rs"` -> `"fooBar"`, `"src/wasm.rs"` -> `"wasm"`.
///
/// # Example
///
/// ```ignore
/// wasm_js_bridge::bundle! {
///     types  = [PredicateOp, PredicateValue, Predicate, Token],
///     fns    = [parse_predicate, parse_predicate_list, eval_predicate, tokenize],
///     aliases = [],
///     opaque  = [],
/// }
/// ```
/// Inject WASM peer imports for npm-packaged dependencies.
///
/// When `WJB_PEER_SHIM` is set (by `wasm-js-bridge build-workspace`), reads
/// the shim file and emits its contents — a `#[wasm_bindgen(module = "...")]
/// extern "C" { ... }` block that imports each peer's exported functions.
///
/// When `WJB_PEER_SHIM` is not set (direct `cargo build`, tests, native builds),
/// expands to nothing. Call once near the top of the crate root.
///
/// ```rust,ignore
/// // In lib.rs or wasm.rs:
/// wasm_js_bridge::wasm_peers!();
/// ```
#[proc_macro]
pub fn wasm_peers(_input: TokenStream) -> TokenStream {
    let shim_path = match std::env::var("WJB_PEER_SHIM") {
        Ok(p) => p,
        Err(_) => return TokenStream::new(),
    };

    let content = match std::fs::read_to_string(&shim_path) {
        Ok(s) => s,
        Err(e) => {
            return syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("Failed to read WJB_PEER_SHIM {shim_path}: {e}"),
            )
            .to_compile_error()
            .into()
        }
    };

    match content.parse::<TokenStream2>() {
        Ok(ts) => ts.into(),
        Err(e) => syn::Error::new(
            proc_macro2::Span::call_site(),
            format!("Invalid peer shim: {e}"),
        )
        .to_compile_error()
        .into(),
    }
}

#[proc_macro]
pub fn bundle(input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(input as BundleArgs);

    // Unique module name per invocation to avoid collision if bundle! is called
    // multiple times in the same crate. Uses a hash of the span's source location.
    let mod_name = {
        let span = proc_macro2::Span::call_site();
        let src = format!("{span:?}");
        let hash: u64 = src
            .bytes()
            .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
        format_ident!("__wjb_bundle_{:016x}", hash)
    };

    // types -> TS and Flow decl calls
    let types = &args.types;

    // fns -> _WASM_JS_BRIDGE_{UPPER} const names
    let export_consts: Vec<syn::Ident> = args
        .fns
        .iter()
        .map(|f| format_ident!("_WASM_JS_BRIDGE_{}", snake_to_screaming(&f.to_string())))
        .collect();

    // aliases
    let alias_items: Vec<TokenStream2> = args
        .aliases
        .iter()
        .map(|(name, target)| quote!(::wasm_js_bridge::TypeAlias { name: #name, target: #target }))
        .collect();

    // opaque types
    let opaque_items: Vec<TokenStream2> = args
        .opaque
        .iter()
        .map(|(name, bound)| match bound {
            Some(b) => quote!(::wasm_js_bridge::OpaqueType { name: #name, bound: Some(#b) }),
            None => quote!(::wasm_js_bridge::OpaqueType { name: #name, bound: None }),
        })
        .collect();

    quote! {
        #[cfg(all(test, feature = "codegen", any(feature = "ts", feature = "flow")))]
        #[allow(non_snake_case)]
        mod #mod_name {
            use super::*;

            #[test]
            fn generate_npm_files() {
                #[cfg(feature = "ts")]
                use ::ts_rs::TS as _;
                #[cfg(feature = "flow")]
                use ::flowjs_rs::Flow as _;

                #[cfg(feature = "ts")]
                let ts_decls: ::std::vec::Vec<::std::string::String> = {
                    let ts_cfg: ::ts_rs::Config = ::std::default::Default::default();
                    ::std::vec![
                        #(<#types as ::ts_rs::TS>::decl(&ts_cfg)),*
                    ]
                };

                #[cfg(feature = "flow")]
                let flow_decls: ::std::vec::Vec<::std::string::String> = {
                    let flow_cfg: ::flowjs_rs::Config = ::std::default::Default::default();
                    ::std::vec![
                        #(<#types as ::flowjs_rs::Flow>::decl(&flow_cfg)),*
                    ]
                };

                let aliases: &[::wasm_js_bridge::TypeAlias] = &[#(#alias_items),*];
                let opaque: &[::wasm_js_bridge::OpaqueType] = &[#(#opaque_items),*];

                let all_fns: &[::wasm_js_bridge::WasmFn] = &[
                    #(#export_consts),*
                ];

                // Output directory: WJB_OUT_DIR env var if set, otherwise CARGO_MANIFEST_DIR.
                let dir = match ::std::env::var("WJB_OUT_DIR") {
                    Ok(d) if !d.is_empty() => ::std::path::PathBuf::from(d),
                    _ => ::std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
                };
                ::std::fs::create_dir_all(&dir).expect("Failed to create output directory");

                // Group functions by source file stem. Each stem produces its own output
                // file set. Every stem receives all type declarations so files are self-contained.
                let mut by_stem: ::std::collections::BTreeMap<
                    ::std::string::String,
                    ::std::vec::Vec<::wasm_js_bridge::WasmFn>,
                > = ::std::default::Default::default();

                // Stem collision guard: two different source file paths must not produce
                // the same camelCase stem, or output files would silently overwrite each other.
                let mut stem_to_file: ::std::collections::BTreeMap<
                    ::std::string::String,
                    &'static str,
                > = ::std::default::Default::default();
                for f in all_fns {
                    let s = ::wasm_js_bridge::file_to_stem(f.file);
                    if let Some(existing_file) = stem_to_file.get(&s) {
                        if *existing_file != f.file {
                            panic!(
                                "wasm-js-bridge bundle!: stem collision — \"{}\" and \"{}\" \
                                 both produce stem \"{}\". Rename one of the source files.",
                                existing_file, f.file, s
                            );
                        }
                    } else {
                        stem_to_file.insert(s.clone(), f.file);
                    }
                    by_stem.entry(s).or_default().push(*f);
                }

                // Fallback: if no fns provided, derive stem from this file
                if by_stem.is_empty() {
                    let stem = ::wasm_js_bridge::file_to_stem(file!());
                    by_stem.insert(stem, ::std::vec::Vec::new());
                }

                for (stem, fns) in &by_stem {
                    #[cfg(feature = "ts")]
                    {
                        let dts = ::wasm_js_bridge::generate_index_dts(&ts_decls, aliases, &[], fns);
                        ::std::fs::write(dir.join(format!("{stem}.d.ts")), dts)
                            .expect("Failed to write .d.ts");
                    }
                    #[cfg(feature = "flow")]
                    {
                        let flow = ::wasm_js_bridge::generate_index_flow(&flow_decls, aliases, &[], fns, opaque);
                        ::std::fs::write(dir.join(format!("{stem}.js.flow")), flow)
                            .expect("Failed to write .js.flow");
                    }
                }
            }
        }
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_to_camel_basic() {
        // Arrange and Act and Assert
        assert_eq!(snake_to_camel("parse_selector"), "parseSelector", "basic");
        assert_eq!(snake_to_camel("select"), "select", "no underscore");
        assert_eq!(
            snake_to_camel("diff_annotations"),
            "diffAnnotations",
            "two words"
        );
        assert_eq!(
            snake_to_camel("extract_aql_symbols"),
            "extractAqlSymbols",
            "three words"
        );
    }

    #[test]
    fn snake_to_screaming_basic() {
        // Arrange and Act and Assert
        assert_eq!(
            snake_to_screaming("parse_selector"),
            "PARSE_SELECTOR",
            "underscore preserved"
        );
        assert_eq!(snake_to_screaming("select"), "SELECT", "single word");
    }

    #[test]
    fn detects_nested_reference_types() {
        // Arrange
        let ty_option_ref: Type = syn::parse_quote!(Option<&str>);
        let ty_vec_ref: Type = syn::parse_quote!(Vec<&MyType>);
        let ty_owned: Type = syn::parse_quote!(Option<String>);

        // Act and Assert
        assert!(
            contains_nested_reference(&ty_option_ref),
            "Option<&T> should be rejected"
        );
        assert!(
            contains_nested_reference(&ty_vec_ref),
            "Vec<&T> should be rejected"
        );
        assert!(
            !contains_nested_reference(&ty_owned),
            "owned generic types should be allowed"
        );
    }
}
