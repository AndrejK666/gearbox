//! What the code can say about a declared plugin relationship.
//!
//! The chain is: a host gear declares an extension point by GTS spec; plugin
//! gears declare that they fill it; the host picks one at runtime by matching a
//! `vendor` string and taking the lowest `priority`. Both sides read that string
//! from their own config, so both have a compiled-in default, and a product that
//! overrides one side and not the other breaks silently -- which is why the
//! defaults are projected.
//!
//! **The role itself is not read here any more.** It used to be: a point was a
//! `pub trait` with `Plugin` in its name, and a plugin a crate implementing one.
//! The corpus broke that five ways -- proxies and built-ins implementing their
//! host's own trait, a trait with no `Plugin` in it, one crate with three gears,
//! two points over one trait, mocks in test support -- so the description now
//! declares the role and this module supplies only what checks it:
//!
//! - [`public_traits`], so a declared trait can be confirmed to exist;
//! - [`implemented_traits`], so a plugin that implements none of its point's
//!   trait can be warned about;
//! - the `vendor`/`priority` defaults, in *both* spellings the tree uses.

use std::collections::BTreeSet;

use crate::scan::RustFile;

/// The `vendor` / `priority` a config type compiles in as its defaults.
///
/// Both are `Option` because a config may default one and require the other,
/// and reporting "no default" is very different from reporting a wrong one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VendorDefault {
    pub vendor: Option<String>,
    pub priority: Option<i64>,
    /// Initializers that are there but could not be read, by field name.
    ///
    /// The distinction `None` alone cannot carry. A missing default is what the
    /// vendor-mismatch check keys on, so "the config declares none" and "the
    /// default is there and this parser could not read it" have to be different
    /// answers -- otherwise a field written as `vendor: some_call()` reports as
    /// a gear that compiled in no vendor at all.
    pub unreadable: Vec<String>,
}

/// Unwrap the string a literal expression yields.
///
/// Accepts the four spellings the tree uses: a bare literal, `.to_owned()`,
/// `.to_string()`, `.into()`, and `String::from("...")`. Anything else is not a
/// compile-time constant and must not be guessed at.
pub(crate) fn str_literal(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Some(s.value()),
        syn::Expr::MethodCall(call) => {
            let method = call.method.to_string();
            if matches!(method.as_str(), "to_owned" | "to_string" | "into") {
                str_literal(&call.receiver)
            } else {
                None
            }
        }
        // `String::from("...")`
        syn::Expr::Call(call) => {
            let is_from = matches!(&*call.func, syn::Expr::Path(p) if p.path.segments.last()
                    .is_some_and(|s| s.ident == "from"));
            if is_from && call.args.len() == 1 {
                str_literal(call.args.first()?)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Unwrap the integer a literal expression yields, including a negative one.
pub(crate) fn int_literal(expr: &syn::Expr) -> Option<i64> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(i),
            ..
        }) => i.base10_parse::<i64>().ok(),
        syn::Expr::Unary(u) if matches!(u.op, syn::UnOp::Neg(_)) => {
            int_literal(&u.expr).map(|v| -v)
        }
        syn::Expr::MethodCall(call) if call.method == "into" => int_literal(&call.receiver),
        _ => None,
    }
}

pub(crate) fn last_segment(path: &syn::Path) -> String {
    path.segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default()
}

/// Every `pub trait` a scanned crate declares, by ident.
///
/// What a declared `extension_point(trait = ...)` is checked against. No
/// filter on the name: the point is declared, so `RateProviderV1` -- which has
/// no `Plugin` in it -- is as good an answer as `AuthNResolverPluginClient`.
#[must_use]
pub fn public_traits(files: &[RustFile]) -> BTreeSet<String> {
    files
        .iter()
        .flat_map(|file| file.ast.items.iter())
        .filter_map(|item| match item {
            syn::Item::Trait(t) if matches!(t.vis, syn::Visibility::Public(_)) => {
                Some(t.ident.to_string())
            }
            _ => None,
        })
        .collect()
}

/// The traits a crate implements outside test code, by last path segment.
///
/// Evidence for a declared `implements`, never the source of it. **Test code does not
/// count**: a host's own tests implement its plugin trait with mocks --
/// usage-collector, license-resolver and credstore all do -- so a file compiled
/// only under `cfg(test)`, or an impl gated that way, is skipped.
#[must_use]
pub fn implemented_traits(files: &[RustFile]) -> BTreeSet<String> {
    let test_only = crate::scan::test_only_files(files);
    files
        .iter()
        .filter(|file| !test_only.contains(&file.relative))
        .flat_map(|file| file.ast.items.iter())
        .filter_map(|item| {
            let syn::Item::Impl(imp) = item else {
                return None;
            };
            if crate::scan::is_test_only(&imp.attrs) {
                return None;
            }
            let (_, path, _) = imp.trait_.as_ref()?;
            Some(last_segment(path))
        })
        .collect()
}

/// The `vendor` / `priority` defaults a crate compiles in.
///
/// **Two spellings exist in the tree and both must be read.** Most configs use
/// `impl Default`, but `oidc-authn-plugin` and `keycloak-idp-plugin` use only
/// `#[serde(default = "default_vendor")]` plus a free function. Reading just the
/// first would silently report "no default" for them -- and a missing default is
/// what the vendor-mismatch check keys on, so that would be a wrong answer, not
/// a gap.
///
/// The same argument is why [`VendorDefault::unreadable`] exists rather than a
/// `Result`: an initializer that is there and cannot be read is a third answer,
/// and it must not collapse into either of the other two. A failure to read one
/// field is no reason to withhold the other, which a `Result` would force.
#[must_use]
pub fn project_vendor_default(files: &[RustFile]) -> VendorDefault {
    let mut from_impl = vendor_from_default_impl(files);
    if from_impl.vendor.is_some() || from_impl.priority.is_some() {
        // Sorted and deduplicated wherever it is returned, so a catalogue built
        // from the same tree is byte-identical.
        from_impl.unreadable.sort();
        from_impl.unreadable.dedup();
        return from_impl;
    }
    // Nothing readable in an `impl Default`: the other spelling may still carry
    // it. An initializer this could not read travels either way, so "the config
    // declares no default" is never reported for a default that is there.
    let mut from_serde = vendor_from_serde_default(files);
    from_serde.unreadable.extend(from_impl.unreadable);
    from_serde.unreadable.sort();
    from_serde.unreadable.dedup();
    from_serde
}

/// The string a `const` initializer yields, through the wrappers
/// [`str_literal`] peels.
///
/// `vendor: DEFAULT_VENDOR.to_owned()` is the shape, and the one `str_literal`
/// has to refuse: a path is a name, not a value.
/// [`crate::cluster::resolve_str_const`] already resolves exactly this for
/// provider names, so the same shape is readable here rather than reported as
/// "no default".
fn str_const(files: &[RustFile], expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Path(p) => crate::cluster::resolve_str_const(files, &last_segment(&p.path)),
        syn::Expr::MethodCall(call)
            if matches!(
                call.method.to_string().as_str(),
                "to_owned" | "to_string" | "into"
            ) =>
        {
            str_const(files, &call.receiver)
        }
        syn::Expr::Call(call) => {
            let is_from = matches!(&*call.func, syn::Expr::Path(p) if p.path.segments.last()
                    .is_some_and(|s| s.ident == "from"));
            if is_from && call.args.len() == 1 {
                str_const(files, call.args.first()?)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Shape 1: `impl Default for XConfig { fn default() -> Self { Self { vendor: .. } } }`
fn vendor_from_default_impl(files: &[RustFile]) -> VendorDefault {
    let mut out = VendorDefault::default();

    for item in files.iter().flat_map(|f| f.ast.items.iter()) {
        let syn::Item::Impl(imp) = item else { continue };
        let implements_default = imp
            .trait_
            .as_ref()
            .is_some_and(|(_, path, _)| last_segment(path) == "Default");
        let on_a_config = matches!(&*imp.self_ty, syn::Type::Path(p)
            if last_segment(&p.path).ends_with("Config"));
        if !implements_default || !on_a_config {
            continue;
        }

        for field in struct_literal_fields(imp) {
            match field.0.as_str() {
                "vendor" if out.vendor.is_none() => {
                    match str_literal(field.1).or_else(|| str_const(files, field.1)) {
                        Some(value) => out.vendor = Some(value),
                        None => out.unreadable.push("vendor".to_owned()),
                    }
                }
                "priority" if out.priority.is_none() => match int_literal(field.1) {
                    Some(value) => out.priority = Some(value),
                    None => out.unreadable.push("priority".to_owned()),
                },
                _ => {}
            }
        }
    }
    out
}

/// The `Self { .. }` fields of a `fn default()` body.
pub(crate) fn struct_literal_fields(imp: &syn::ItemImpl) -> Vec<(String, &syn::Expr)> {
    let Some(func) = imp.items.iter().find_map(|i| match i {
        syn::ImplItem::Fn(f) if f.sig.ident == "default" => Some(f),
        _ => None,
    }) else {
        return Vec::new();
    };
    let Some(syn::Stmt::Expr(syn::Expr::Struct(lit), None)) = func.block.stmts.last() else {
        return Vec::new();
    };
    lit.fields
        .iter()
        .filter_map(|f| match &f.member {
            syn::Member::Named(ident) => Some((ident.to_string(), &f.expr)),
            syn::Member::Unnamed(_) => None,
        })
        .collect()
}

/// Shape 2: `#[serde(default = "default_vendor")]` on the field, plus the fn.
fn vendor_from_serde_default(files: &[RustFile]) -> VendorDefault {
    let mut out = VendorDefault::default();

    for item in files.iter().flat_map(|f| f.ast.items.iter()) {
        let syn::Item::Struct(s) = item else { continue };
        for field in &s.fields {
            let Some(ident) = field.ident.as_ref().map(ToString::to_string) else {
                continue;
            };
            if ident != "vendor" && ident != "priority" {
                continue;
            }
            let read = serde_default_fn(&field.attrs);
            let Some(fn_name) = read.name else {
                // A `serde` attribute this could not read to its end may have
                // carried the `default = "fn"` past the point it stopped, and
                // that is not the same answer as a field with no default.
                if read.truncated {
                    out.unreadable.push(ident);
                }
                continue;
            };
            let Some(body) = free_fn_body(files, &fn_name) else {
                out.unreadable.push(ident);
                continue;
            };
            match ident.as_str() {
                "vendor" if out.vendor.is_none() => {
                    match str_literal(body).or_else(|| str_const(files, body)) {
                        Some(value) => out.vendor = Some(value),
                        None => out.unreadable.push(ident),
                    }
                }
                "priority" if out.priority.is_none() => match int_literal(body) {
                    Some(value) => out.priority = Some(value),
                    None => out.unreadable.push(ident),
                },
                _ => {}
            }
        }
    }
    out
}

/// The string a config field at `path` defaults to -- `"idp.vendor"` reads
/// the `vendor` default of whatever type the config's `idp` field has.
///
/// **What it walks.** The root is the one `*Config` struct with a field named
/// after the first segment; every segment before the last is a field whose
/// type (through `Option<..>`) names the next struct; the last is read from
/// that struct's `impl Default` or its `#[serde(default = "fn")]` -- the two
/// spellings [`project_vendor_default`] reads, for the same reason.
///
/// # Errors
/// A sentence saying where the walk stopped: no root, more than one, a field
/// whose type is not a struct in the crate, or a default that is not a
/// readable string. A declared selector that cannot be read is a description
/// error, not "no default".
pub fn project_field_str_default(files: &[RustFile], path: &str) -> Result<Option<String>, String> {
    let segments: Vec<&str> = path.split('.').collect();
    let structs: std::collections::BTreeMap<String, &syn::ItemStruct> = files
        .iter()
        .flat_map(|f| f.ast.items.iter())
        .filter_map(|item| match item {
            syn::Item::Struct(s) => Some((s.ident.to_string(), s)),
            _ => None,
        })
        .collect();
    let field_of = |s: &syn::ItemStruct, name: &str| -> Option<syn::Field> {
        s.fields
            .iter()
            .find(|f| f.ident.as_ref().is_some_and(|i| i == name))
            .cloned()
    };

    let roots: Vec<&str> = structs
        .iter()
        .filter(|(name, s)| name.ends_with("Config") && field_of(s, segments[0]).is_some())
        .map(|(name, _)| name.as_str())
        .collect();
    let mut owner = match roots.as_slice() {
        [one] => (*one).to_owned(),
        [] => {
            return Err(format!(
                "no `*Config` struct in the crate has a field `{}`",
                segments[0]
            ));
        }
        many => {
            return Err(format!(
                "`{}` is a field of several config structs ({}); the selector cannot say which",
                segments[0],
                many.join(", ")
            ));
        }
    };

    for segment in &segments[..segments.len() - 1] {
        let field = structs
            .get(&owner)
            .and_then(|s| field_of(s, segment))
            .ok_or_else(|| format!("`{owner}` has no field `{segment}`"))?;
        let next = struct_type_name(&field.ty)
            .filter(|name| structs.contains_key(name))
            .ok_or_else(|| format!("`{owner}.{segment}` is not a struct declared in the crate"))?;
        owner = next;
    }

    let last = segments[segments.len() - 1];
    let field = structs
        .get(&owner)
        .and_then(|s| field_of(s, last))
        .ok_or_else(|| format!("`{owner}` has no field `{last}`"))?;

    // `impl Default for <owner>` first, as `project_vendor_default` does.
    for item in files.iter().flat_map(|f| f.ast.items.iter()) {
        let syn::Item::Impl(imp) = item else { continue };
        let is_default = imp
            .trait_
            .as_ref()
            .is_some_and(|(_, p, _)| last_segment(p) == "Default");
        let on_owner =
            matches!(&*imp.self_ty, syn::Type::Path(p) if last_segment(&p.path) == owner);
        if !(is_default && on_owner) {
            continue;
        }
        if let Some((_, expr)) = struct_literal_fields(imp)
            .into_iter()
            .find(|(n, _)| n == last)
        {
            return str_literal(expr)
                .or_else(|| str_const(files, expr))
                .map(Some)
                .ok_or_else(|| format!("`{owner}::default().{last}` is not a readable string"));
        }
    }
    let read = serde_default_fn(&field.attrs);
    if let Some(name) = read.name {
        let body = free_fn_body(files, &name).ok_or_else(|| {
            format!("`{owner}.{last}` defaults through `{name}`, which is not found")
        })?;
        return str_literal(body)
            .or_else(|| str_const(files, body))
            .map(Some)
            .ok_or_else(|| format!("`{name}()` does not return a readable string"));
    }
    Ok(None)
}

/// The struct a field's type names, through one `Option<..>`.
fn struct_type_name(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(p) = ty else { return None };
    let segment = p.path.segments.last()?;
    if segment.ident == "Option" {
        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
            && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
        {
            return struct_type_name(inner);
        }
        return None;
    }
    Some(segment.ident.to_string())
}

/// What reading `#[serde(default = "name")]` off a field yielded.
pub(crate) struct SerdeDefault {
    pub name: Option<String>,
    /// True when a `serde` attribute could not be read to its end.
    ///
    /// `parse_nested_meta` stops at the first meta form it cannot model and
    /// everything after it in the same attribute is lost with it, so a
    /// `default = "fn"` written after such a form is never seen. Dropping the
    /// error made that look like "no default fn" -- and a missing default is
    /// exactly what the vendor-mismatch check keys on, so it came back as a
    /// wrong answer rather than a gap.
    pub truncated: bool,
}

/// The function name in `#[serde(default = "name")]`, if present.
pub(crate) fn serde_default_fn(attrs: &[syn::Attribute]) -> SerdeDefault {
    let mut truncated = false;
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        let mut found = None;
        let read = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("default")
                && let Ok(value) = meta.value()
                && let Ok(lit) = value.parse::<syn::LitStr>()
            {
                found = Some(lit.value());
            } else if meta.input.peek(syn::Token![=]) {
                // Consume `rename = "..."`, `skip_serializing_if = "..."` and
                // any other `key = expr` so they do not abort the rest of the
                // list before `default = "fn"` is seen.
                let _ = meta.value()?.parse::<syn::Expr>()?;
            }
            Ok(())
        });
        // A bare `#[serde(default)]` also lands here, which is why this is a
        // signal and not an error: the caller decides whether a truncated read
        // matters for what it was looking for.
        truncated |= read.is_err() && found.is_none();
        if found.is_some() {
            return SerdeDefault {
                name: found,
                truncated,
            };
        }
    }
    SerdeDefault {
        name: None,
        truncated,
    }
}

/// The trailing expression of a free `fn name() -> _`.
pub(crate) fn free_fn_body<'a>(files: &'a [RustFile], name: &str) -> Option<&'a syn::Expr> {
    // Inline `mod` blocks included: the config projection's root discovery walks
    // them, so a lookup that stopped at the file's top level would disagree with
    // it about which functions exist.
    files
        .iter()
        .flat_map(crate::scan::items)
        .find_map(|item| match item {
            syn::Item::Fn(f) if f.sig.ident == name => match f.block.stmts.last() {
                Some(syn::Stmt::Expr(expr, None)) => Some(expr),
                _ => None,
            },
            _ => None,
        })
}

#[cfg(test)]
#[path = "plugin_tests.rs"]
mod plugin_tests;
