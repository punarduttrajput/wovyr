//! `#[derive(Tool)]` (SBX-303, [tool-framework
//! spec](../../../docs/04-agent-framework/tool-framework.md)): generates the
//! declarative boilerplate an [`apex_tools::Tool`] implementation needs — a
//! [`ToolMetadata`](apex_tools::ToolMetadata) constructor and a JSON Schema (via
//! `schemars`) — from struct-level attributes and a separately-declared
//! parameters struct that derives `schemars::JsonSchema` (+ `serde::Deserialize`
//! for the actual parsing).
//!
//! What it does **not** generate: `Tool::execute` — that's the tool's actual
//! logic, and there's nothing to derive it from. The author still writes
//! `execute()`, but through a generated typed-parse helper instead of a
//! hand-rolled `request.parameters.get("x").and_then(Value::as_str)` chain, and
//! with no `json!({...})` schema literal to keep in sync with the params type by
//! hand.
//!
//! ```ignore
//! use apex_tool_macros::Tool;
//! use apex_tools::{ToolContext, ToolError, ToolRequest, ToolResponse};
//! use schemars::JsonSchema;
//! use serde::Deserialize;
//!
//! /// Typed, schema-derived parameters — no hand-written JSON Schema.
//! #[derive(Deserialize, JsonSchema)]
//! struct GreetParams {
//!     /// Name to greet.
//!     name: String,
//!     /// Number of times to repeat the greeting.
//!     #[serde(default = "default_count")]
//!     count: u32,
//! }
//! fn default_count() -> u32 {
//!     1
//! }
//!
//! #[derive(Tool)]
//! #[tool(
//!     id = "greet",
//!     version = "1.0.0",
//!     category = "utility",
//!     description = "Greet someone by name.",
//!     params = GreetParams,
//! )]
//! struct GreetTool;
//!
//! #[async_trait::async_trait]
//! impl apex_tools::Tool for GreetTool {
//!     fn metadata(&self) -> apex_tools::ToolMetadata {
//!         Self::__tool_metadata()
//!     }
//!     fn input_schema(&self) -> serde_json::Value {
//!         Self::__tool_input_schema()
//!     }
//!     async fn execute(
//!         &self,
//!         _ctx: &ToolContext,
//!         request: ToolRequest,
//!     ) -> Result<ToolResponse, ToolError> {
//!         // Typed, not `request.parameters.get("name").and_then(Value::as_str)`.
//!         let params = Self::__tool_parse_params(&request)?;
//!         Ok(ToolResponse::success(serde_json::json!({
//!             "greeting": params.name,
//!         })))
//!     }
//! }
//! ```
//!
//! `permissions` is optional (`#[tool(..., permissions = ["net.egress"])]`);
//! omitted, the generated metadata declares none, matching
//! `ToolMetadata::new`'s own default.

use proc_macro::TokenStream;
use quote::quote;
use syn::{
    DeriveInput, Expr, ExprArray, ExprLit, Lit, LitStr, Path, Token, parse::Parse,
    parse::ParseStream, parse_macro_input,
};

/// Parsed contents of the required `#[tool(...)]` attribute.
struct ToolAttr {
    id: LitStr,
    version: LitStr,
    category: LitStr,
    description: LitStr,
    params: Path,
    permissions: Vec<LitStr>,
}

impl Parse for ToolAttr {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut id = None;
        let mut version = None;
        let mut category = None;
        let mut description = None;
        let mut params = None;
        let mut permissions = Vec::new();

        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "id" => id = Some(input.parse::<LitStr>()?),
                "version" => version = Some(input.parse::<LitStr>()?),
                "category" => category = Some(input.parse::<LitStr>()?),
                "description" => description = Some(input.parse::<LitStr>()?),
                "params" => params = Some(input.parse::<Path>()?),
                "permissions" => {
                    let arr: ExprArray = input.parse()?;
                    for el in arr.elems {
                        match el {
                            Expr::Lit(ExprLit {
                                lit: Lit::Str(s), ..
                            }) => permissions.push(s),
                            other => {
                                return Err(syn::Error::new_spanned(
                                    other,
                                    "`permissions` entries must be string literals",
                                ));
                            }
                        }
                    }
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown `#[tool(...)]` key `{other}` (expected one of: id, \
                             version, category, description, params, permissions)"
                        ),
                    ));
                }
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(ToolAttr {
            id: id.ok_or_else(|| input.error("missing required `id = \"...\"`"))?,
            version: version.ok_or_else(|| input.error("missing required `version = \"...\"`"))?,
            category: category
                .ok_or_else(|| input.error("missing required `category = \"...\"`"))?,
            description: description
                .ok_or_else(|| input.error("missing required `description = \"...\"`"))?,
            params: params.ok_or_else(|| input.error("missing required `params = <Type>`"))?,
            permissions,
        })
    }
}

/// Derive the `#[tool(...)]`-driven metadata/schema/typed-parse boilerplate.
/// See the crate-level docs for the full picture and what still must be
/// hand-written.
#[proc_macro_derive(Tool, attributes(tool))]
pub fn derive_tool(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let Some(tool_attr) = input.attrs.iter().find(|a| a.path().is_ident("tool")) else {
        return syn::Error::new_spanned(
            &input,
            "#[derive(Tool)] requires a `#[tool(id = \"...\", version = \"...\", \
             category = \"...\", description = \"...\", params = <Type>)]` attribute",
        )
        .to_compile_error()
        .into();
    };

    let parsed: ToolAttr = match tool_attr.parse_args() {
        Ok(p) => p,
        Err(e) => return e.to_compile_error().into(),
    };

    let ToolAttr {
        id,
        version,
        category,
        description,
        params,
        permissions,
    } = parsed;

    // An empty array literal (`[]`) can't infer its element type on its own;
    // an explicit `Vec::<&str>::new()` disambiguates when no permissions are
    // declared.
    let permissions_expr = if permissions.is_empty() {
        quote! { ::std::vec::Vec::<&str>::new() }
    } else {
        quote! { [#(#permissions),*] }
    };

    let expanded = quote! {
        impl #impl_generics #ident #ty_generics #where_clause {
            /// Generated by `#[derive(Tool)]` (SBX-303) — what `Tool::metadata`
            /// should delegate to.
            pub fn __tool_metadata() -> ::apex_tools::ToolMetadata {
                ::apex_tools::ToolMetadata::new(#id, #version, #category, #description)
                    .with_permissions(#permissions_expr)
            }

            /// Generated by `#[derive(Tool)]` — what `Tool::input_schema` should
            /// delegate to, derived from `#params`'s `schemars::JsonSchema` impl
            /// rather than a hand-written `json!({...})` literal.
            pub fn __tool_input_schema() -> ::serde_json::Value {
                ::serde_json::to_value(::schemars::schema_for!(#params))
                    .expect("a derived JsonSchema always serializes to JSON")
            }

            /// Generated by `#[derive(Tool)]` — typed parameter parsing,
            /// replacing a hand-written `.get().and_then()` chain. Fails with
            /// `ToolError::Validation` on a schema mismatch, never a panic.
            pub fn __tool_parse_params(
                request: &::apex_tools::ToolRequest,
            ) -> ::std::result::Result<#params, ::apex_tools::ToolError> {
                ::serde_json::from_value(request.parameters.clone()).map_err(|e| {
                    ::apex_tools::ToolError::Validation(format!("invalid parameters: {e}"))
                })
            }
        }
    };

    expanded.into()
}
