//! Tests for plugin projection.
//!
//! The fixtures reproduce the shapes found in `gears-rust`; the real-tree tests
//! then prove the fixtures have not drifted from what they mirror.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::{Path, PathBuf};

use super::*;
use crate::attribute::files_owned_by;
use crate::scan::scan_crate;
use crate::test_corpus::require;

fn file(src: &str) -> RustFile {
    RustFile {
        path: PathBuf::from("fixture.rs"),
        relative: PathBuf::from("fixture.rs"),
        ast: syn::parse_file(src).expect("fixture parses"),
    }
}

fn tree(rel: &str) -> Option<Vec<RustFile>> {
    let dir = crate::test_corpus::corpus(rel)?;
    Some(scan_crate(&dir).unwrap_or_else(|e| panic!("scan {rel}: {e}")))
}

fn at(relative: &str, src: &str) -> RustFile {
    RustFile {
        path: PathBuf::from(relative),
        relative: PathBuf::from(relative),
        ast: syn::parse_file(src).expect("fixture parses"),
    }
}

fn names(set: &std::collections::BTreeSet<String>) -> Vec<&str> {
    set.iter().map(String::as_str).collect()
}

// ---------------------------------------------------------------- traits

#[test]
fn every_public_trait_is_listed_whatever_its_name() {
    // The point is declared now, so the reader does not filter on `Plugin`:
    // `RateProviderV1` is as valid a trait to name as any.
    let sdk = file(
        "pub trait AuthNResolverPluginClient {}
         pub trait RateProviderV1 {}
         trait Private {}
         pub(crate) trait CrateOnly {}",
    );
    assert_eq!(
        names(&public_traits(&[sdk])),
        ["AuthNResolverPluginClient", "RateProviderV1"]
    );
}

#[test]
fn the_real_ledger_sdk_declares_the_rate_provider_trait() {
    // The trait bss-rate-provider's sources implement lives in *another* gear's
    // SDK, and has no `Plugin` in its name -- the case the heuristic missed.
    let sdk = require!(tree("gears/bss/ledger/ledger-sdk"));
    assert!(public_traits(&sdk).contains("RateProviderV1"));
}

// ---------------------------------------------------------------- impls

#[test]
fn implemented_traits_are_read_by_last_segment() {
    let files = [file(
        "pub struct A; impl sdk::AuthNResolverPluginClient for A {}
         impl Default for A { fn default() -> Self { A } }",
    )];
    assert_eq!(
        names(&implemented_traits(&files)),
        ["AuthNResolverPluginClient", "Default"]
    );
}

#[test]
fn a_mock_in_test_support_is_not_evidence() {
    // The usage-collector shape: the host's own tests implement its plugin
    // trait, in a file that `domain/mod.rs` pulls in under `cfg(test)`.
    let crate_files = [
        at("lib.rs", "pub mod domain;"),
        at(
            "domain/mod.rs",
            "pub mod service;\n#[cfg(test)]\npub mod test_support;",
        ),
        at("domain/service.rs", "pub struct Service;"),
        at(
            "domain/test_support.rs",
            "pub struct MockPlugin; impl UsageCollectorPluginV1 for MockPlugin {}",
        ),
    ];
    assert!(!implemented_traits(&crate_files).contains("UsageCollectorPluginV1"));
}

#[test]
fn a_test_module_named_by_path_and_its_children_are_test_code() {
    // `#[path = "service_tests.rs"] mod service_tests;` beside `service.rs`,
    // which is how most of the corpus spells it, and a submodule of a test
    // module, which is test code by inheritance rather than by its own gate.
    let files = [
        at(
            "lib.rs",
            "pub mod domain; #[cfg(all(test, feature = \"x\"))] mod probe;",
        ),
        at(
            "domain/mod.rs",
            "pub mod service; #[cfg(test)] pub(crate) mod test_support;",
        ),
        at(
            "domain/service.rs",
            "#[cfg(test)]\n#[path = \"service_tests.rs\"]\nmod service_tests;",
        ),
        at("domain/service_tests.rs", ""),
        at("domain/test_support/mod.rs", "pub mod idp;"),
        at("domain/test_support/idp.rs", ""),
        at("probe.rs", ""),
    ];
    let test_only: Vec<String> = crate::scan::test_only_files(&files)
        .into_iter()
        .map(|p| p.display().to_string())
        .filter(|p| files.iter().any(|f| f.relative.display().to_string() == *p))
        .collect();
    assert_eq!(
        test_only,
        [
            "domain/service_tests.rs",
            "domain/test_support/idp.rs",
            "domain/test_support/mod.rs",
            "probe.rs",
        ]
    );
}

#[test]
fn a_gate_that_also_admits_ordinary_builds_is_evidence() {
    // `any(test, ...)` compiles the impl outside tests too, so it still counts.
    let files = [at(
        "lib.rs",
        "pub struct X; #[cfg(any(test, feature = \"mock\"))] impl P for X {}",
    )];
    assert!(implemented_traits(&files).contains("P"));
    let gated = [at("lib.rs", "pub struct X; #[cfg(test)] impl P for X {}")];
    assert!(!implemented_traits(&gated).contains("P"));
}

#[test]
fn real_hosts_with_mock_plugins_implement_nothing_of_their_own_point() {
    // Each of these implements its own plugin trait in test support only.
    for (host, point) in [
        (
            "gears/system/usage-collector/usage-collector",
            "UsageCollectorPluginV1",
        ),
        (
            "gears/system/license-resolver/license-resolver",
            "LicenseResolverPluginClient",
        ),
        ("gears/credstore/credstore", "CredStorePluginClientV1"),
    ] {
        let files = require!(tree(host));
        assert!(
            !implemented_traits(&files).contains(point),
            "{host}: a test mock is not evidence"
        );
    }
}

// ---------------------------------------------------------------- ownership

#[test]
fn a_crate_with_several_gears_splits_its_files_by_the_deepest_attribute() {
    // The mini-chat shape: the host's attribute at the top, each plugin's in
    // its own directory, and a file under a plugin's directory is that
    // plugin's alone.
    let gear = |name: &str| format!("#[toolkit::gear(name = \"{name}\")] pub struct G;");
    let files = [
        at("gear.rs", &gear("host")),
        at("lib.rs", "pub mod gear; pub mod plugins;"),
        at("plugins/audit/gear.rs", &gear("audit")),
        at("plugins/audit/service.rs", "impl AuditPlugin for S {}"),
        at("plugins/policy/gear.rs", &gear("policy")),
        at("plugins/policy/service.rs", "impl PolicyPlugin for S {}"),
    ];
    let rel = |f: &RustFile| f.relative.display().to_string();

    let host = files_owned_by(&files, Path::new("gear.rs")).expect("narrowed");
    assert_eq!(
        host.iter().map(rel).collect::<Vec<_>>(),
        ["gear.rs", "lib.rs"]
    );

    let audit = files_owned_by(&files, Path::new("plugins/audit/gear.rs")).expect("narrowed");
    assert_eq!(
        audit.iter().map(rel).collect::<Vec<_>>(),
        ["plugins/audit/gear.rs", "plugins/audit/service.rs"]
    );
    assert!(!implemented_traits(&audit).contains("PolicyPlugin"));
}

#[test]
fn a_single_gear_crate_is_not_copied() {
    let files = [
        at("gear.rs", "#[toolkit::gear(name = \"only\")] pub struct G;"),
        at("lib.rs", ""),
    ];
    assert!(files_owned_by(&files, Path::new("gear.rs")).is_none());
}

#[test]
fn real_mini_chat_plugins_each_implement_only_their_own_trait() {
    let files = require!(tree("gears/mini-chat/mini-chat"));
    let audit = files_owned_by(&files, Path::new("infra/plugins/static_audit/gear.rs"))
        .expect("mini-chat declares several gears");
    let traits = implemented_traits(&audit);
    assert!(traits.contains("MiniChatAuditPluginClientV1"));
    assert!(!traits.contains("MiniChatModelPolicyPluginClientV1"));

    let host = files_owned_by(&files, Path::new("gear.rs")).expect("narrowed");
    let traits = implemented_traits(&host);
    assert!(!traits.contains("MiniChatAuditPluginClientV1"));
    assert!(!traits.contains("MiniChatModelPolicyPluginClientV1"));
}

// ---------------------------------------------------------------- defaults

#[test]
fn vendor_default_from_an_impl_default_block() {
    let src = r#"
        pub struct StaticAuthNPluginConfig { pub vendor: String, pub priority: i16 }
        impl Default for StaticAuthNPluginConfig {
            fn default() -> Self {
                Self { vendor: "constructorfabric".to_owned(), priority: 100 }
            }
        }
    "#;
    assert_eq!(
        project_vendor_default(&[file(src)]),
        VendorDefault {
            vendor: Some("constructorfabric".to_owned()),
            priority: Some(100),
            unreadable: Vec::new(),
        }
    );
}

#[test]
fn vendor_default_from_a_serde_default_fn() {
    // The shape `oidc-authn-plugin` and `keycloak-idp-plugin` use. Reading only
    // `impl Default` would report "no default" for them, which is a wrong answer
    // rather than a gap: the mismatch check keys on the default.
    let src = r#"
        pub struct OidcAuthNGearConfig {
            #[serde(default = "default_vendor")]
            pub vendor: String,
            #[serde(default = "default_priority")]
            pub priority: u32,
        }
        fn default_vendor() -> String { "constructorfabric".to_owned() }
        fn default_priority() -> u32 { 100 }
    "#;
    assert_eq!(
        project_vendor_default(&[file(src)]),
        VendorDefault {
            vendor: Some("constructorfabric".to_owned()),
            priority: Some(100),
            unreadable: Vec::new(),
        }
    );
}

#[test]
fn a_config_with_no_default_reports_none() {
    let src = r"
        pub struct Thing { pub vendor: String }
    ";
    assert_eq!(
        project_vendor_default(&[file(src)]),
        VendorDefault::default()
    );
}

/// The shape `cluster::resolve_str_const` already reads for provider names.
/// Reporting it as "no default" would be a wrong answer, not a gap: a missing
/// default is what the vendor-mismatch check keys on.
#[test]
fn a_vendor_default_behind_a_const_resolves() {
    let src = r#"
        pub const DEFAULT_VENDOR: &str = "constructorfabric";
        pub struct XConfig { pub vendor: String }
        impl Default for XConfig {
            fn default() -> Self {
                Self { vendor: DEFAULT_VENDOR.to_owned() }
            }
        }
    "#;
    let got = project_vendor_default(&[file(src)]);
    assert_eq!(got.vendor.as_deref(), Some("constructorfabric"));
    assert!(got.unreadable.is_empty(), "got {:?}", got.unreadable);
}

/// The third answer. "The config declares no default" and "the default is there
/// and this could not read it" are different facts, and collapsing them made a
/// gear that compiles in a vendor look like one that does not.
#[test]
fn an_unreadable_initializer_is_recorded_rather_than_reported_as_absent() {
    let src = r"
        pub struct XConfig { pub vendor: String, pub priority: i16 }
        impl Default for XConfig {
            fn default() -> Self {
                Self { vendor: vendor_from_env(), priority: compute_priority() }
            }
        }
    ";
    let got = project_vendor_default(&[file(src)]);
    assert_eq!(got.vendor, None);
    assert_eq!(got.priority, None);
    // Sorted, so a catalogue built from the same tree is byte-identical.
    assert_eq!(got.unreadable, ["priority", "vendor"]);
}

/// `parse_nested_meta` stops at the first form it cannot model and loses every
/// later item in the same attribute, so the `default = "fn"` written after a
/// nested `rename(..)` was never seen and read as "no default fn".
#[test]
fn a_truncated_serde_attribute_does_not_read_as_no_default() {
    let src = r#"
        pub struct XConfig {
            #[serde(rename(serialize = "v", deserialize = "vendor"), default = "default_vendor")]
            pub vendor: String,
        }
        fn default_vendor() -> String { "constructorfabric".to_owned() }
    "#;
    let got = project_vendor_default(&[file(src)]);
    assert_eq!(
        got.unreadable,
        ["vendor"],
        "the attribute could not be read to its end, so `None` here is a gap and \
         not an answer: {got:?}"
    );
}

#[test]
fn real_vendor_defaults_are_read_in_both_shapes() {
    // static-authn uses `impl Default`; oidc uses `#[serde(default = ...)]`.
    // Both must come back with a vendor, or the mismatch check is blind on one.
    let static_authn = require!(tree(
        "gears/system/authn-resolver/plugins/static-authn-plugin"
    ));
    assert_eq!(
        project_vendor_default(&static_authn).vendor.as_deref(),
        Some("constructorfabric")
    );

    let oidc = require!(tree(
        "gears/system/authn-resolver/plugins/oidc-authn-plugin"
    ));
    assert_eq!(
        project_vendor_default(&oidc).vendor.as_deref(),
        Some("constructorfabric"),
        "the #[serde(default = ...)] shape must be read too"
    );

    let host = require!(tree("gears/system/authn-resolver/authn-resolver"));
    assert_eq!(
        project_vendor_default(&host).vendor.as_deref(),
        Some("constructorfabric"),
        "the host's selector default is read the same way"
    );
}

#[test]
fn account_managements_selector_is_misread_from_its_other_role() {
    // **Recorded, not endorsed.** This test used to be called "the defaults
    // really do disagree" and to call the disagreement the motivating case for
    // GBX0512. It was a misreading: account-management selects its IdP plugin by
    // `idp.vendor`, whose default is "cf" -- the same as static-idp-plugin's --
    // and the "constructorfabric" read here is `tr_plugin.vendor`, the vendor of
    // its *other* role, as a tenant-resolver plugin. `project_vendor_default`
    // takes the first `*Config` with a `vendor` default, and this crate has two.
    //
    // It mattered little while account-management was mis-classified as a
    // plugin. As a declared host it would make a product listing static-idp-plugin
    // under it fail GBX0512 for no reason. The fix is for the point to name its
    // selector field; until then this pins the wrong answer so the day it
    // changes is a decision.
    let host = require!(tree("gears/system/account-management/account-management"));
    let static_idp = require!(tree(
        "gears/system/account-management/plugins/static-idp-plugin"
    ));
    assert_eq!(
        project_vendor_default(&host).vendor.as_deref(),
        Some("constructorfabric"),
        "still read from `TrPluginConfig`"
    );
    assert_eq!(
        project_vendor_default(&static_idp).vendor.as_deref(),
        Some("cf"),
        "which the real selector, `idp.vendor`, matches by default"
    );
}

// ---------------------------------------------------------------- selector

/// account-management's shape: two `vendor` fields, one it selects by.
const TWO_VENDORS: &str = r#"
#[derive(Default)]
pub struct AmConfig { pub idp: IdpConfig, pub tr_plugin: TrPluginConfig, pub keycloak: Option<KeycloakConfig> }
pub struct IdpConfig { pub vendor: String }
impl Default for IdpConfig { fn default() -> Self { Self { vendor: "cf".to_owned() } } }
pub struct TrPluginConfig { pub vendor: String }
impl Default for TrPluginConfig { fn default() -> Self { Self { vendor: "constructorfabric".to_owned() } } }
pub struct KeycloakConfig { #[serde(default = "default_vendor")] pub vendor: String }
fn default_vendor() -> String { "keycloak".to_owned() }
"#;

#[test]
fn a_selector_path_reads_the_default_of_the_field_it_names() {
    let files = [file(TWO_VENDORS)];
    assert_eq!(
        project_field_str_default(&files, "idp.vendor")
            .unwrap()
            .as_deref(),
        Some("cf")
    );
    assert_eq!(
        project_field_str_default(&files, "tr_plugin.vendor")
            .unwrap()
            .as_deref(),
        Some("constructorfabric")
    );
    // Through `Option<..>`, and through `#[serde(default = "fn")]`.
    assert_eq!(
        project_field_str_default(&files, "keycloak.vendor")
            .unwrap()
            .as_deref(),
        Some("keycloak")
    );
}

#[test]
fn a_selector_path_that_leads_nowhere_says_where_it_stopped() {
    let files = [file(TWO_VENDORS)];
    let err = project_field_str_default(&files, "idp.nope").unwrap_err();
    assert!(err.contains("`IdpConfig` has no field `nope`"), "{err}");
    let err = project_field_str_default(&files, "missing.vendor").unwrap_err();
    assert!(err.contains("no `*Config` struct"), "{err}");
    let err = project_field_str_default(&files, "idp.vendor.deeper").unwrap_err();
    assert!(err.contains("not a struct"), "{err}");
}

#[test]
fn the_real_account_management_selects_by_idp_vendor() {
    // The corpus case this exists for: the first `vendor` default in the crate
    // is `tr_plugin`'s, the one the runtime selects the IdP plugin by is not.
    let files = require!(tree("gears/system/account-management/account-management"));
    assert_eq!(
        project_field_str_default(&files, "idp.vendor")
            .unwrap()
            .as_deref(),
        Some("cf")
    );
}
