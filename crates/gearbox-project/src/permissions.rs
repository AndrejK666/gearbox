//! The abstract permissions a gear's code names (gears-rust
//! `cpt-cf-adr-operation-permissions` §6).
//!
//! Three spellings, each read as a string literal:
//!
//! * `.permission("settings.values.write")` on an `OperationBuilder` chain;
//! * `#[permission("settings.values.write")]` on a `rest_contract` trait method,
//!   which lives in the gear's SDK crate;
//! * `permissions.holds(&ctx, "settings.values.write")` or `.decide(&ctx, "...")`
//!   (and the `PermissionMap::holds` / `PermissionMap::decide` call forms), where
//!   code decides itself.
//!
//! Anything other than a literal -- a constant, a variable, a helper -- cannot be
//! read without compiling the gear, so it is reported by location rather than
//! assumed absent. Test code is skipped, as `client_uses` skips it.

use std::collections::BTreeSet;

use syn::spanned::Spanned as _;
use syn::visit::Visit;

use crate::scan::{RustFile, is_test_only, test_only_files};

/// What a crate's code names, and where it names something unreadable.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProjectedPermissions {
    /// Every permission written as a literal.
    pub names: BTreeSet<String>,
    /// `file:line` of every permission given another way.
    pub unreadable: Vec<String>,
}

/// The permissions `gear_files` and `sdk_files` name, outside test code.
#[must_use]
pub fn project_permissions(
    gear_files: &[RustFile],
    sdk_files: &[RustFile],
) -> ProjectedPermissions {
    let mut out = ProjectedPermissions::default();
    for files in [gear_files, sdk_files] {
        let tests = test_only_files(files);
        for file in files.iter().filter(|f| !tests.contains(&f.relative)) {
            let mut visitor = Names {
                file: file.relative.display().to_string(),
                out: &mut out,
            };
            visitor.visit_file(&file.ast);
        }
    }
    out.unreadable.sort();
    out.unreadable.dedup();
    out
}

struct Names<'a> {
    file: String,
    out: &'a mut ProjectedPermissions,
}

impl Names<'_> {
    fn take(&mut self, expr: &syn::Expr) {
        match expr {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(name),
                ..
            }) => {
                self.out.names.insert(name.value());
            }
            other => self.unreadable(other.span()),
        }
    }

    fn unreadable(&mut self, span: proc_macro2::Span) {
        self.out
            .unreadable
            .push(format!("{}:{}", self.file, span.start().line));
    }
}

/// `#[test]`, `#[tokio::test]` and the like.
fn is_test_fn(attrs: &[syn::Attribute]) -> bool {
    is_test_only(attrs)
        || attrs
            .iter()
            .any(|a| a.path().segments.last().is_some_and(|s| s.ident == "test"))
}

impl<'ast> Visit<'ast> for Names<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !is_test_only(&item.attrs) {
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if !is_test_only(&item.attrs) {
            syn::visit::visit_item_impl(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if !is_test_fn(&item.attrs) {
            syn::visit::visit_item_fn(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !is_test_fn(&item.attrs) {
            syn::visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        for attr in item
            .attrs
            .iter()
            .filter(|a| a.path().is_ident("permission"))
        {
            match attr.parse_args::<syn::LitStr>() {
                Ok(name) => {
                    self.out.names.insert(name.value());
                }
                Err(_) => self.unreadable(attr.span()),
            }
        }
        syn::visit::visit_trait_item_fn(self, item);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        let method = call.method.to_string();
        if method == "permission" && call.args.len() == 1 {
            self.take(&call.args[0]);
        } else if (method == "holds" || method == "decide") && call.args.len() == 2 {
            self.take(&call.args[1]);
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        // `PermissionMap::holds(&permissions, &ctx, "...")`.
        if let syn::Expr::Path(path) = &*call.func
            && path
                .path
                .segments
                .last()
                .is_some_and(|s| s.ident == "holds" || s.ident == "decide")
            && path.path.segments.len() > 1
            && call.args.len() == 3
        {
            self.take(&call.args[2]);
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        // A statement-like body (`tokio::spawn(async { .. })` bodies, `vec![]`
        // blocks), or an argument list such as `ensure!(cond, "message")`.
        if let Ok(block) = mac.parse_body_with(syn::Block::parse_within) {
            for stmt in &block {
                self.visit_stmt(stmt);
            }
        } else if let Ok(args) = mac.parse_body_with(
            syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated,
        ) {
            for arg in &args {
                self.visit_expr(arg);
            }
        }
        syn::visit::visit_macro(self, mac);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn file(source: &str) -> RustFile {
        RustFile {
            path: PathBuf::from("/c/src/lib.rs"),
            relative: PathBuf::from("lib.rs"),
            ast: syn::parse_file(source).unwrap(),
        }
    }

    #[test]
    fn reads_every_literal_spelling() {
        let gear = file(
            r#"
            fn routes(router: Router) -> Router {
                OperationBuilder::post("/v")
                    .authenticated()
                    .permission("settings.values.write")
                    .register(router, openapi)
            }
            fn outbox(permissions: &PermissionMap, ctx: &SecurityContext) -> bool {
                permissions.holds(ctx, "settings.outbox.deliver")
                    || PermissionMap::holds(permissions, ctx, "settings.outbox.read")
                    || permissions.decide(ctx, "oagw.proxy.invoke").is_ok()
            }
            "#,
        );
        let sdk = file(
            r#"
            #[rest_contract(base_path = "/s")]
            pub trait SettingsApiRest: SettingsApi {
                #[get("/v")]
                #[permission("settings.values.read")]
                async fn get(&self, ctx: SecurityContext) -> Result<V, E>;
            }
            "#,
        );
        let projected = project_permissions(&[gear], &[sdk]);
        assert_eq!(
            projected
                .names
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "oagw.proxy.invoke",
                "settings.outbox.deliver",
                "settings.outbox.read",
                "settings.values.read",
                "settings.values.write"
            ]
        );
        assert!(
            projected.unreadable.is_empty(),
            "{:?}",
            projected.unreadable
        );
    }

    /// `anyhow::ensure!(permissions.holds(&ctx, "..."), "...")`: an argument
    /// list, not statements, is still read.
    #[test]
    fn reads_a_check_inside_a_macro_argument_list() {
        let gear = file(
            r#"
            fn start(permissions: &PermissionMap, ctx: &SecurityContext) -> anyhow::Result<()> {
                anyhow::ensure!(
                    permissions.holds(ctx, "settings.events.produce") && ctx.is_user(),
                    "the producer lacks `settings.events.produce`"
                );
                Ok(())
            }
            "#,
        );
        let projected = project_permissions(&[gear], &[]);
        assert!(
            projected.names.contains("settings.events.produce"),
            "{projected:?}"
        );
    }

    #[test]
    fn a_non_literal_is_reported_where_it_is() {
        let gear = file(
            "const WRITE: &str = \"settings.values.write\";\n\
             fn routes(b: Builder) -> Builder { b.permission(WRITE) }\n",
        );
        let projected = project_permissions(&[gear], &[]);
        assert!(projected.names.is_empty());
        assert_eq!(projected.unreadable, ["lib.rs:2"]);
    }

    #[test]
    fn test_code_and_unrelated_calls_are_not_read() {
        let gear = file(
            r#"
            fn elect(record: &Record, set: &Set) -> bool { set.holds(record) }
            #[cfg(test)]
            mod tests {
                fn f(b: Builder) -> Builder { b.permission("tests.only") }
            }
            "#,
        );
        assert_eq!(
            project_permissions(&[gear], &[]),
            ProjectedPermissions::default()
        );
    }
}
