//! Projecting a gear's runtime configuration surface out of the struct it
//! deserializes into.
//!
//! **Why this is projected and not declared.** A field's name, type, whether it
//! is required and what it defaults to are all stated in Rust, and ADR
//! `cpt-gearbox-adr-macro-projected-catalogue` gives Rust every fact Rust
//! already expresses. A `gear.gdl` declares only the disjoint remainder, which
//! for configuration is a product judgement Rust has no way to hold: *which of
//! these fields is worth putting in front of an integrator*.
//!
//! **Who reads the result.** Two consumers, deliberately one model. The Studio
//! renders a typed control per field; the chart generator owes
//! `cpt-gearbox-fr-values-schema` a JSON Schema constraining "field exists,
//! field type, whether field is required" -- which is this struct, field for
//! field. Deriving them separately is how `Helm values` ends up in the drift
//! list vision §8 exists to keep it out of. [`ConfigField::secret`] serves the
//! second requirement of that pair, `cpt-gearbox-fr-no-secrets-in-values`: a
//! generator cannot write `existingSecret` for a field it cannot tell apart
//! from a hostname.
//!
//! **What is deliberately not projected.** Anything whose value is not a scalar
//! -- nested structures, lists, `#[serde(flatten)]` maps, a field with a custom
//! codec -- is reported as [`ConfigFieldType::Complex`] rather than guessed at.
//! `ApiGatewayConfig` nests seven further structures and `OidcAuthNGearConfig`
//! more than ten; a form for those is a different project, and the honest answer
//! meanwhile is to say the shape is not scalar rather than to invent a control
//! that would write the wrong thing.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::plugin::{
    free_fn_body, int_literal, last_segment, serde_default_fn, str_literal, struct_literal_fields,
};
use gearbox_ir::ConfigFieldType;

use crate::scan::RustFile;

/// One field of a gear's configuration, as Rust states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigField {
    /// The name **on the wire**, after `#[serde(rename)]` and `rename_all`.
    ///
    /// Not the Rust ident: `TenantConfig::tenant_type` is written `type` in
    /// every YAML in the corpus, and a control labelled `tenant_type` would
    /// write a key the gear never reads.
    pub name: String,
    pub ty: ConfigFieldType,
    /// Whether omitting the field is an error.
    ///
    /// False when serde would fill it: a container or field `#[serde(default)]`,
    /// a `default = "fn"`, or an `Option<T>` -- serde's `missing_field` succeeds
    /// for a type that deserializes from nothing, so an `Option` is optional
    /// without saying so.
    pub required: bool,
    /// The compiled-in default, when it is a literal this can read.
    pub default: Option<serde_json::Value>,
    /// The field's doc comment, which is the only prose an operator gets.
    pub doc: Option<String>,
    /// The value is a credential slot: any `secrecy` wrapper.
    ///
    /// Read from the field's type and from nothing else, and never overridden by
    /// what the wire shape turns out to be. This is the only gate
    /// `gearbox-engine` has for keeping a credential out of the generated
    /// `ConfigMap` (`cpt-gearbox-fr-no-secrets-in-values`), so a field whose shape
    /// cannot be rendered is still a secret if its type says so -- the
    /// unreadable shape is a reason to report [`ConfigFieldType::Complex`], not
    /// a reason to forget what the field holds.
    pub secret: bool,
    /// One level of a struct-typed field: the fields of the struct its type
    /// names, when this crate declares it with named fields. Empty otherwise,
    /// and always empty one level down. The control stays
    /// [`ConfigFieldType::Complex`]; this is what a check reads to say that a
    /// set `jwt = {...}` lacks the required `claim_mapping`.
    pub fields: Vec<ConfigField>,
    /// Path relative to the crate's `src/`, and the line of the field.
    pub relative: PathBuf,
    pub line: usize,
}

/// Why the configuration struct could not be identified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigRootError {
    /// Several distinct types are deserialized as this gear's config, so which
    /// one *is* the configuration has no single answer.
    Ambiguous { roots: Vec<String> },
}

/// Why a configuration struct's fields could not be projected.
///
/// Three answers that used to be one empty vector. A caller could not tell a
/// root naming nothing in this crate from a struct with no named keys, so
/// `gearbox-engine` had to emit the merged sentence "`{root}` declares no
/// configuration fields, or this crate does not declare it" -- and the root can
/// be an operator-supplied string via `config_schema = config(rust = ...)`, so
/// which of the two happened is exactly what the reader needs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigFieldsError {
    #[error("`{root}` is not a struct this crate declares")]
    RootNotFound { root: String },

    /// A tuple or unit struct. `ApiContractsConfig` is exactly this.
    #[error("`{root}` is not a struct with named fields, so it has no keys an operator can set")]
    NoNamedFields { root: String },

    /// A `#[serde(...)]` this parser could not read to its end.
    ///
    /// `parse_nested_meta` stops at the first meta form it cannot model and
    /// every later item in the same attribute is lost with it, so
    /// `#[serde(rename(serialize = "a", deserialize = "b"), skip)]` -- legal
    /// serde -- used to leave `skip` false and project a field serde never
    /// reads into the values schema.
    #[error("a `#[serde(...)]` on `{root}`{} could not be read to its end", .field.as_ref().map(|f| format!("::{f}")).unwrap_or_default())]
    UnreadableSerdeAttribute {
        root: String,
        /// The field it sits on, or `None` for the container's own attribute.
        field: Option<String>,
    },
}

/// The methods on `GearCtx` that deserialize a gear's configuration.
///
/// The join key between a gear and its config type, and the only one there is:
/// the `Gear` trait has no associated `Config`, so nothing in the type system
/// records the pairing. Each of these appears at most once per gear in the tree.
const CONFIG_METHODS: &[&str] = &[
    "config",
    "config_or_default",
    "config_expanded",
    "config_expanded_or_default",
];

/// The type a gear deserializes its configuration into, by ident.
///
/// **Two spellings, and both are in the tree.** `api-gateway` turbofishes the
/// type onto the call; `grpc-hub` and the nine others annotate the binding and
/// let inference carry it:
///
/// ```rust,ignore
/// ctx.config_or_default::<crate::config::ApiGatewayConfig>()   // turbofish
/// let cfg: GrpcHubConfig = ctx.config_or_default()?;           // annotation
/// ```
///
/// Reading only the first would report "no configuration" for ten of the eleven
/// configured gears. Looking for `src/config.rs` instead would miss `grpc-hub`,
/// whose struct lives in `src/gear.rs`.
///
/// # Errors
/// Returns [`ConfigRootError::Ambiguous`] when the crate deserializes more than
/// one distinct type as its config.
pub fn project_config_root(files: &[RustFile]) -> Result<Option<String>, ConfigRootError> {
    let mut found = Vec::new();
    for file in files {
        let mut visitor = RootVisitor {
            files,
            found: &mut found,
        };
        syn::visit::visit_file(&mut visitor, &file.ast);
    }
    found.sort();
    found.dedup();

    match found.len() {
        0 => Ok(None),
        1 => Ok(found.into_iter().next()),
        _ => Err(ConfigRootError::Ambiguous { roots: found }),
    }
}

struct RootVisitor<'a> {
    files: &'a [RustFile],
    found: &'a mut Vec<String>,
}

/// Whether a `ctx.config*()` call appears anywhere inside an expression.
///
/// A search rather than a match on one shape: `?`, `.await`, `.unwrap()` and
/// `.expect(..)` all sit between the binding and the call in the tree, and
/// enumerating the wrappers would leave the next one unread.
fn calls_config(expr: &syn::Expr) -> bool {
    struct Search(bool);
    impl<'ast> syn::visit::Visit<'ast> for Search {
        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            if CONFIG_METHODS.iter().any(|m| call.method == *m) {
                self.0 = true;
            }
            syn::visit::visit_expr_method_call(self, call);
        }
    }
    let mut search = Search(false);
    syn::visit::visit_expr(&mut search, expr);
    search.0
}

impl<'ast> syn::visit::Visit<'ast> for RootVisitor<'_> {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if CONFIG_METHODS.iter().any(|m| call.method == *m)
            && let Some(turbofish) = call.turbofish.as_ref()
            && let Some(syn::GenericArgument::Type(ty)) = turbofish.args.first()
            && let Some(ident) = type_ident(ty)
        {
            self.found.push(ident);
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        // The annotation counts only when it names a struct this crate declares.
        // Because the call is *searched for* rather than matched, an expression
        // like `ctx.config::<X>().unwrap().len()` would otherwise record the
        // binding's `usize`; a config type is a struct, and `usize` is not one.
        if let syn::Pat::Type(pat) = &local.pat
            && let Some(init) = local.init.as_ref()
            && calls_config(&init.expr)
            && let Some(ident) = type_ident(&pat.ty)
            && find_struct(self.files, &ident).is_some()
        {
            self.found.push(ident);
        }
        syn::visit::visit_local(self, local);
    }
}

/// The last path segment of a type, which is the ident a struct is found by.
fn type_ident(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(p) => Some(last_segment(&p.path)),
        syn::Type::Reference(r) => type_ident(&r.elem),
        _ => None,
    }
}

/// The serde attributes this projection reads, from one container or field.
#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent serde attribute; collapsing them into \
              an enum would claim they are mutually exclusive, and `skip` with \
              `default` is a pair that occurs"
)]
struct SerdeAttrs {
    default_bare: bool,
    default_fn: Option<String>,
    rename: Option<String>,
    rename_all: Option<String>,
    /// A custom codec: the wire shape is no longer the Rust type's.
    with: bool,
    flatten: bool,
    skip: bool,
    /// `transparent`, `from` or `try_from` on a container: what is on the
    /// wire is another type's shape, not this struct's named keys.
    shape_elsewhere: bool,
    /// True when an attribute could not be read to its end, so the flags above
    /// are what was seen rather than what was written.
    partial: bool,
}

fn serde_attrs(attrs: &[syn::Attribute]) -> SerdeAttrs {
    let read = serde_default_fn(attrs);
    let mut out = SerdeAttrs {
        default_fn: read.name,
        partial: read.truncated,
        ..SerdeAttrs::default()
    };
    for attr in attrs.iter().filter(|a| a.path().is_ident("serde")) {
        // `parse_nested_meta` stops at the first form it does not understand and
        // loses every later item in the same attribute with it. Recorded rather
        // than dropped: a truncated read is not the same as a fully-read
        // attribute that happened to say nothing.
        let read = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("default") {
                // Bare `default` is followed by a comma or by the end of the
                // list; `default = "fn"` by an `=`. Testing for an empty input
                // instead would miss `#[serde(default, deny_unknown_fields)]`,
                // which is how most of the corpus spells it.
                if meta.input.peek(syn::Token![=]) {
                    // Consume `default = "fn"` so a later `rename` / `skip` is
                    // still seen. The fn name itself is collected by
                    // `serde_default_fn`.
                    let _ = meta.value()?.parse::<syn::Expr>()?;
                } else {
                    out.default_bare = true;
                }
            } else if meta.path.is_ident("flatten") {
                out.flatten = true;
            } else if meta.path.is_ident("transparent") {
                out.shape_elsewhere = true;
            } else if meta.path.is_ident("from") || meta.path.is_ident("try_from") {
                out.shape_elsewhere = true;
                let _ = meta.value()?.parse::<syn::Expr>()?;
            } else if meta.path.is_ident("skip") {
                out.skip = true;
            } else if meta.path.is_ident("with") {
                out.with = true;
                // `with = "path::to::codec"`. The path itself is not read --
                // only that there is one -- but it still has to be consumed, or
                // the walk stops here and everything after it is lost.
                let _ = meta.value()?.parse::<syn::Expr>()?;
            } else if meta.path.is_ident("rename")
                && let Ok(value) = meta.value()
                && let Ok(lit) = value.parse::<syn::LitStr>()
            {
                out.rename = Some(lit.value());
            } else if meta.path.is_ident("rename_all")
                && let Ok(value) = meta.value()
                && let Ok(lit) = value.parse::<syn::LitStr>()
            {
                out.rename_all = Some(lit.value());
            } else if meta.input.peek(syn::Token![=]) {
                // Consume `serialize_with = "..."`, `skip_serializing_if = "..."`
                // and any other `key = expr` so they do not abort the rest of
                // the list -- an unknown form used to drop `rename` / `secret`
                // that followed it.
                let _ = meta.value()?.parse::<syn::Expr>()?;
            }
            Ok(())
        });
        out.partial |= read.is_err();
    }
    out
}

/// Apply serde's `rename_all` to one identifier.
///
/// The cases are serde's, spelled as serde spells them. An unknown value leaves
/// the name alone rather than guessing: a wrong rename produces a key the gear
/// silently never reads, which is worse than no rename at all.
fn rename_all(value: &str, ident: &str) -> String {
    use heck::{
        ToKebabCase as _, ToLowerCamelCase as _, ToShoutyKebabCase as _, ToShoutySnakeCase as _,
        ToSnakeCase as _, ToUpperCamelCase as _,
    };
    match value {
        "lowercase" => ident.to_lowercase(),
        "UPPERCASE" => ident.to_uppercase(),
        "PascalCase" => ident.to_upper_camel_case(),
        "camelCase" => ident.to_lower_camel_case(),
        "snake_case" => ident.to_snake_case(),
        "SCREAMING_SNAKE_CASE" => ident.to_shouty_snake_case(),
        "kebab-case" => ident.to_kebab_case(),
        "SCREAMING-KEBAB-CASE" => ident.to_shouty_kebab_case(),
        _ => ident.to_owned(),
    }
}

/// The named structs and enums a scan declares, by ident.
///
/// Built once per projection instead of scanning `files` per field.
/// `find_item` was a linear pass over every item of every file, run once for
/// every config field that is not a known scalar and again in `enum_default` for
/// one with a default -- so a struct like `OidcAuthNGearConfig` with ten-plus
/// non-scalar fields cost ten-plus full passes over the crate's AST for what is
/// a lookup by ident.
type ItemIndex<'a> = HashMap<String, &'a syn::Item>;

/// Index the named items in `files`, inline `mod` blocks included.
fn item_index<'a>(files: &'a [RustFile]) -> ItemIndex<'a> {
    let mut out: ItemIndex<'a> = HashMap::new();
    for item in files.iter().flat_map(crate::scan::items) {
        let ident = match item {
            syn::Item::Struct(s) => s.ident.to_string(),
            syn::Item::Enum(e) => e.ident.to_string(),
            _ => continue,
        };
        // First wins, matching the linear scan this replaces. Two items of one
        // name in one crate is legal across modules, and picking the later one
        // would change which of them a field's type resolves to.
        out.entry(ident).or_insert(item);
    }
    out
}

/// The struct an ident names, together with the file it was found in.
///
/// Descends into inline `mod` blocks, because [`project_config_root`]'s visitor
/// does: a root declared inside one used to be dropped by the annotation form's
/// `find_struct` gate and then yield no fields under the turbofish form, so the
/// two halves of one projection disagreed about which structs exist.
fn find_struct<'a>(
    files: &'a [RustFile],
    ident: &str,
) -> Option<(&'a RustFile, &'a syn::ItemStruct)> {
    files.iter().find_map(|file| {
        crate::scan::items(file)
            .into_iter()
            .find_map(|item| match item {
                syn::Item::Struct(s) if s.ident == ident => Some((file, s)),
                _ => None,
            })
    })
}

/// The wire spellings of a unit-only enum, or `None` if it is not one.
///
/// A data-carrying variant means the value is not a scalar, so there is no
/// control for it and `Complex` is the truthful answer.
fn variant_names(item: &syn::ItemEnum) -> Option<Vec<(String, String)>> {
    let container = serde_attrs(&item.attrs);
    item.variants
        .iter()
        .map(|variant| {
            if !matches!(variant.fields, syn::Fields::Unit) {
                return None;
            }
            let attrs = serde_attrs(&variant.attrs);
            let ident = variant.ident.to_string();
            let wire = attrs.rename.unwrap_or_else(|| {
                container
                    .rename_all
                    .as_deref()
                    .map_or(ident.clone(), |rule| rename_all(rule, &ident))
            });
            Some((ident, wire))
        })
        .collect()
}

/// The wire spellings of a unit-only enum, or `None` if it is not one.
fn unit_enum_variants(item: &syn::ItemEnum) -> Option<Vec<String>> {
    Some(
        variant_names(item)?
            .into_iter()
            .map(|(_, wire)| wire)
            .collect(),
    )
}

/// Whether a path type is `Option<T>`, and its `T`.
fn option_inner(ty: &syn::Type) -> Option<&syn::Type> {
    let syn::Type::Path(p) = ty else { return None };
    let segment = p.path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        syn::GenericArgument::Type(inner) => Some(inner),
        _ => None,
    })
}

/// The first generic argument of a path type, if it has one.
fn first_type_argument(ty: &syn::Type) -> Option<&syn::Type> {
    let syn::Type::Path(p) = ty else { return None };
    let syn::PathArguments::AngleBracketed(args) = &p.path.segments.last()?.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        syn::GenericArgument::Type(inner) => Some(inner),
        _ => None,
    })
}

/// Classify a field's Rust type into the control it admits.
fn classify(ty: &syn::Type, items: &ItemIndex<'_>) -> (ConfigFieldType, bool) {
    let Some(ident) = type_ident(ty) else {
        return (ConfigFieldType::Complex, false);
    };
    match ident.as_str() {
        // A path is a string on the wire, and an operator types one. So is a
        // GTS type id: `gts::GtsTypeId` deserializes from its string form.
        "String" | "str" | "PathBuf" | "Path" | "GtsTypeId" => (ConfigFieldType::Str, false),
        // Every `secrecy` wrapper, not the one alias. The point of all of them
        // is that the value is a credential, and a field typed `Secret<String>`
        // or `SecretBox<str>` used to fall through to the item lookup and come
        // back `Complex` with `secret: false` -- so a properly wrapped
        // credential projected as an ordinary value and the generator wrote it
        // into the ConfigMap in plaintext.
        "SecretString" => (ConfigFieldType::Str, true),
        "Secret" | "SecretBox" => (
            // The control follows the wrapped type, so `Secret<u16>` is still a
            // number box; a wrapper with no argument to read is a string, which
            // is what every spelling in the tree amounts to.
            first_type_argument(ty).map_or(ConfigFieldType::Str, |inner| classify(inner, items).0),
            true,
        ),
        // A list of bytes has no control, but it is still a credential.
        "SecretVec" | "SecretSlice" => (ConfigFieldType::Complex, true),
        "bool" => (ConfigFieldType::Bool, false),
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => (ConfigFieldType::Int, false),
        "f32" | "f64" => (ConfigFieldType::Float, false),
        _ => match items.get(&ident) {
            Some(syn::Item::Enum(e)) => unit_enum_variants(e)
                .map_or((ConfigFieldType::Complex, false), |v| {
                    (ConfigFieldType::Enum { variants: v }, false)
                }),
            // A struct, a collection, or a type from a crate outside the scan.
            // All three are "no control", and saying so beats guessing.
            _ => (ConfigFieldType::Complex, false),
        },
    }
}

/// The literal an expression carries, for the field defaults worth showing.
fn literal_value(expr: &syn::Expr) -> Option<serde_json::Value> {
    if let Some(s) = str_literal(expr) {
        return Some(serde_json::Value::String(s));
    }
    if let Some(i) = int_literal(expr) {
        return Some(serde_json::Value::from(i));
    }
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Bool(b),
            ..
        }) => Some(serde_json::Value::Bool(b.value)),
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Float(f),
            ..
        }) => f
            .base10_parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(serde_json::Value::Number),
        // Paths are deliberately absent. `None` is the *absence* of a default,
        // not the string "None"; a `const` is a name this cannot resolve, and
        // reporting the identifier as though it were the value is a lie a
        // placeholder would then show. An enum variant is resolved by the caller,
        // which knows the field's type and so can spell it as the wire does.
        _ => None,
    }
}

/// The wire spelling of `variant` in the enum named `ty`, if it is a unit-only
/// enum this scan can see.
///
/// Needed because a default is written in Rust (`AuthNMode::AcceptAll`) and read
/// in YAML (`accept_all`). Showing the Rust ident as a placeholder would offer a
/// value the gear rejects.
fn enum_default(
    items: &ItemIndex<'_>,
    ty: &syn::Type,
    expr: &syn::Expr,
) -> Option<serde_json::Value> {
    let syn::Expr::Path(path) = expr else {
        return None;
    };
    let variant = last_segment(&path.path);
    let ident = type_ident(ty)?;
    let syn::Item::Enum(item) = items.get(&ident)? else {
        return None;
    };
    let wire = variant_names(item)?;
    wire.into_iter()
        .find(|(rust, _)| rust == &variant)
        .map(|(_, wire)| serde_json::Value::String(wire))
}

/// The `Self { .. }` of `impl Default for <ident>`, as field name to expression.
fn default_impl_fields<'a>(files: &'a [RustFile], ident: &str) -> Vec<(String, &'a syn::Expr)> {
    // Inline `mod` blocks included, for the reason `find_struct` walks them: a
    // config struct declared in one would otherwise find its own `Default` impl
    // invisible and report every field as having no default.
    files
        .iter()
        .flat_map(crate::scan::items)
        .filter_map(|item| match item {
            syn::Item::Impl(imp) => Some(imp),
            _ => None,
        })
        .filter(|imp| {
            imp.trait_
                .as_ref()
                .is_some_and(|(_, path, _)| last_segment(path) == "Default")
                && matches!(&*imp.self_ty, syn::Type::Path(p) if last_segment(&p.path) == ident)
        })
        .flat_map(struct_literal_fields)
        .collect()
}

/// The doc comment on an item, as one line of prose.
fn doc_of(attrs: &[syn::Attribute]) -> Option<String> {
    let lines: Vec<String> = attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(|a| match &a.meta {
            syn::Meta::NameValue(nv) => str_literal(&nv.value),
            _ => None,
        })
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect();
    (!lines.is_empty()).then(|| lines.join(" "))
}

/// Every scalar-or-not field of the gear's configuration struct.
///
/// `root` is the struct ident, from [`project_config_root`] or from a
/// description's `config_schema = config(rust = ...)` escape hatch. An `Ok` that
/// is empty means every field the struct declares is `#[serde(skip)]`, which is
/// a third answer again: the struct is there, it has named keys, and none of
/// them is configuration.
///
/// # Errors
/// Returns [`ConfigFieldsError`] when `root` names nothing this crate declares,
/// when it names something with no named fields, or when a `#[serde(...)]` on it
/// could not be read to its end.
pub fn project_config_fields(
    files: &[RustFile],
    root: &str,
) -> Result<Vec<ConfigField>, ConfigFieldsError> {
    project_struct_fields(files, root, true)
}

/// [`project_config_fields`], descending one level into struct-typed fields
/// when `nested`.
fn project_struct_fields(
    files: &[RustFile],
    root: &str,
    nested: bool,
) -> Result<Vec<ConfigField>, ConfigFieldsError> {
    let Some((file, item)) = find_struct(files, root) else {
        return Err(ConfigFieldsError::RootNotFound {
            root: root.to_owned(),
        });
    };
    let syn::Fields::Named(named) = &item.fields else {
        return Err(ConfigFieldsError::NoNamedFields {
            root: root.to_owned(),
        });
    };

    let container = serde_attrs(&item.attrs);
    if container.partial {
        return Err(ConfigFieldsError::UnreadableSerdeAttribute {
            root: root.to_owned(),
            field: None,
        });
    }
    // One level down, a struct whose wire shape is another type's has no keys
    // of its own to check: `TopicSettingsMap` is a transparent map, and its
    // field `entries` is never written.
    if !nested && container.shape_elsewhere {
        return Ok(Vec::new());
    }
    let defaults = default_impl_fields(files, root);
    let items = item_index(files);

    let mut out = Vec::new();
    for field in &named.named {
        let Some(ident) = field.ident.as_ref().map(ToString::to_string) else {
            continue;
        };
        let attrs = serde_attrs(&field.attrs);
        // Refused rather than read past. `skip` is the flag a truncated read
        // most often loses, and a field serde never deserializes would then be
        // projected as configuration and land in the values schema.
        if attrs.partial {
            return Err(ConfigFieldsError::UnreadableSerdeAttribute {
                root: root.to_owned(),
                field: Some(ident),
            });
        }
        if attrs.skip {
            continue;
        }

        let name = attrs.rename.clone().unwrap_or_else(|| {
            container
                .rename_all
                .as_deref()
                .map_or(ident.clone(), |rule| rename_all(rule, &ident))
        });

        let optional = option_inner(&field.ty);
        let (classified, secret) = classify(optional.unwrap_or(&field.ty), &items);
        // A custom codec or a flattened map: the wire shape is not this type's,
        // so there is nothing honest to render. That overrides the *control* and
        // nothing else -- `secret` comes from the type either way, since it is
        // the only gate keeping a credential out of the generated ConfigMap.
        let ty = if attrs.with || attrs.flatten {
            ConfigFieldType::Complex
        } else {
            classified
        };

        let required = !container.default_bare
            && !attrs.default_bare
            && attrs.default_fn.is_none()
            && optional.is_none();

        let default_expr = attrs
            .default_fn
            .as_deref()
            .and_then(|name| free_fn_body(files, name))
            .or_else(|| {
                defaults
                    .iter()
                    .find(|(f, _)| *f == ident)
                    .map(|(_, expr)| *expr)
            });
        // Dropped for a secret. A compiled-in credential read off
        // `#[serde(default = "default_password")]` used to land verbatim in the
        // catalogue, which walks straight around the `secret` flag computed
        // above: the redaction in `gearbox-engine`'s `secrets.rs` rewrites
        // configured values, not projected defaults.
        let default = if secret {
            None
        } else {
            default_expr.and_then(|expr| {
                literal_value(expr)
                    .or_else(|| enum_default(&items, optional.unwrap_or(&field.ty), expr))
            })
        };

        // One level into a struct this crate declares, so a check can name a
        // required key inside a set value. A nested struct that cannot be read
        // contributes nothing rather than failing the gear's whole schema.
        let fields = if nested && ty == ConfigFieldType::Complex && !secret {
            type_ident(optional.unwrap_or(&field.ty))
                .filter(|ident| matches!(items.get(ident), Some(syn::Item::Struct(_))))
                .and_then(|ident| project_struct_fields(files, &ident, false).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        out.push(ConfigField {
            name,
            ty,
            required,
            default,
            doc: doc_of(&field.attrs),
            secret,
            fields,
            relative: file.relative.clone(),
            line: field.ident.as_ref().map_or(0, |i| i.span().start().line),
        });
    }
    Ok(out)
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;
