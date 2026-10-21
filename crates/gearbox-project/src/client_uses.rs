//! The contract traits a crate's code actually resolves from the `ClientHub`.
//!
//! A gear says what it consumes with `#[toolkit::consumes]` and `consume(...)`;
//! what it *calls* is `hub.get::<dyn Trait>()` in a function body, and nothing
//! made the two agree. This reads the second so the catalogue can compare it
//! with the first (Phase 4 of the service-deployment roadmap, test case G16).
//!
//! Syntax, not resolution: the trait is named by its last path segment, the way
//! `#[toolkit::consumes(contract = ...)]` is read. Test code is skipped -- a test
//! resolves whatever it registered: test-only files, and inside the others any
//! `#[cfg(test)]` module, impl or function and any `#[test]`-style function.

use std::collections::BTreeSet;

use syn::visit::Visit;

use crate::scan::{RustFile, is_test_only, test_only_files};

/// Methods that resolve a client from the hub by its trait object type.
const RESOLVERS: [&str; 3] = ["get", "try_get", "get_scoped"];

/// The trait idents `get::<dyn Trait>()` (and its siblings) name anywhere in
/// the crate's non-test code.
#[must_use]
pub fn project_client_uses(files: &[RustFile]) -> BTreeSet<String> {
    let tests = test_only_files(files);
    let mut visitor = Uses::default();
    for file in files.iter().filter(|f| !tests.contains(&f.relative)) {
        visitor.visit_file(&file.ast);
    }
    visitor.traits
}

#[derive(Default)]
struct Uses {
    traits: BTreeSet<String>,
}

/// `#[test]`, `#[tokio::test]` and the like: a test function, whatever runs it.
fn is_test_fn(attrs: &[syn::Attribute]) -> bool {
    is_test_only(attrs)
        || attrs
            .iter()
            .any(|a| a.path().segments.last().is_some_and(|s| s.ident == "test"))
}

impl<'ast> Visit<'ast> for Uses {
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

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if RESOLVERS.contains(&call.method.to_string().as_str())
            && let Some(turbofish) = &call.turbofish
        {
            for arg in &turbofish.args {
                if let syn::GenericArgument::Type(syn::Type::TraitObject(object)) = arg {
                    self.traits
                        .extend(object.bounds.iter().find_map(|bound| match bound {
                            syn::TypeParamBound::Trait(t) => {
                                t.path.segments.last().map(|s| s.ident.to_string())
                            }
                            _ => None,
                        }));
                }
            }
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    // Macro bodies are tokens, not expressions: a `get::<dyn X>()` inside
    // `tokio::spawn(async move { ... })` is an expression, but one inside a
    // `format!`-style macro is not, and would need re-parsing. Read the
    // statement-like ones as a block.
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if let Ok(block) = mac.parse_body_with(syn::Block::parse_within) {
            for stmt in &block {
                self.visit_stmt(stmt);
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
    fn reads_the_trait_a_body_resolves() {
        let uses = project_client_uses(&[file(
            r"
            fn wire(hub: &ClientHub) {
                let api = hub.get::<dyn api_sdk::PaymentApi>().unwrap();
                let other = hub.try_get::<dyn AuthZResolverApi + Send>();
                let not_a_trait_object = hub.get::<Config>();
                tokio::spawn(async move { hub.get::<dyn InSpawn>() });
            }
            ",
        )]);
        assert_eq!(
            uses.into_iter().collect::<Vec<_>>(),
            ["AuthZResolverApi", "InSpawn", "PaymentApi"]
        );
    }

    #[test]
    fn inline_test_code_is_not_production_consumption() {
        let uses = project_client_uses(&[file(
            r"
            fn serve(hub: &ClientHub) { hub.get::<dyn Served>(); }

            #[cfg(test)]
            mod tests {
                fn helper(hub: &ClientHub) { hub.get::<dyn InTestModule>(); }
            }

            #[test]
            fn a_test(hub: &ClientHub) { hub.get::<dyn InTestFn>(); }

            #[tokio::test]
            async fn an_async_test(hub: &ClientHub) { hub.get::<dyn InAsyncTest>(); }

            #[cfg(test)]
            fn gated(hub: &ClientHub) { hub.get::<dyn InGatedFn>(); }

            struct S;
            #[cfg(test)]
            impl S { fn m(hub: &ClientHub) { hub.get::<dyn InGatedImpl>(); } }
            ",
        )]);
        assert_eq!(uses.into_iter().collect::<Vec<_>>(), ["Served"]);
    }
}
