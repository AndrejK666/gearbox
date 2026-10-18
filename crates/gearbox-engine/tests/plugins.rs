//! Plugin selection, resolved against the real catalogue.
//!
//! These need both halves -- the catalogue says what each gear fills, the
//! product says what is linked -- so they live here rather than in either crate
//! alone. The product is evaluated from a string literal; only the catalogue
//! comes from the sibling checkout.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::PathBuf;

use gearbox_engine::{SourceRoot, check_plugins, load_catalogue};
use gearbox_gdl::{FileIdentity, GdlEngine};
use gearbox_ir::{Catalogue, DiagnosticCode, Diagnostics, RelPath, SourceId};

#[path = "common/corpus.rs"]
mod corpus_support;

fn gears_rust() -> Option<PathBuf> {
    corpus_support::root()
}

fn catalogue() -> Option<Catalogue> {
    let root = gears_rust()?;
    let source = SourceRoot::open(SourceId::new("gears-rust").unwrap(), root)
        .unwrap_or_else(|error| panic!("the discovered corpus must be readable: {error}"));
    Some(load_catalogue(&[source]).catalogue)
}

macro_rules! require {
    () => {
        match catalogue() {
            Some(c) => c,
            None => {
                eprintln!("skipping: ../gears-rust not present");
                return;
            }
        }
    };
}

/// A product with one host and whatever plugin clause is under test.
fn product(profiles: &str, default: &str, gears: &str) -> String {
    format!(
        r#"
product(
    id = "t", version = "0.1.0",
    sources = [source(id = "gears-rust", at = path("../gears-rust"))],
    profiles = [{profiles}],
    default_profile = "{default}",
    gears = [{gears}],
)
"#
    )
}

/// Evaluate the product, then check it against the catalogue.
fn check(catalogue: &Catalogue, src: &str) -> (Vec<DiagnosticCode>, String) {
    let identity = FileIdentity {
        uri: "file:///t/product.gdl".to_owned(),
        source: SourceId::new("product").unwrap(),
        gdl_path: RelPath::new("product.gdl").unwrap(),
        load_paths: None,
    };
    let outcome = GdlEngine::new().eval_product(&identity, src);
    let intent = outcome.value.unwrap_or_else(|| {
        panic!(
            "product did not evaluate: {:?}",
            outcome.diagnostics.as_slice()
        )
    });

    let mut diagnostics = Diagnostics::new();
    check_plugins(
        catalogue,
        &intent,
        "file:///t/product.gdl",
        &mut diagnostics,
    );
    diagnostics.finish();

    let codes = diagnostics.iter().map(|d| d.code).collect();
    let messages = diagnostics
        .iter()
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join(" | ");
    (codes, messages)
}

const AUTHN: &str = r#"use_gear("authn-resolver", source = "gears-rust""#;

#[test]
fn a_filled_point_is_clean() {
    let cat = require!();
    let (codes, messages) = check(
        &cat,
        &product(
            r#"embedded(id = "dev")"#,
            "dev",
            &format!(r#"{AUTHN}, plugins = [plugin("static-authn-plugin")])"#),
        ),
    );
    assert!(codes.is_empty(), "{codes:?} {messages}");
}

#[test]
fn an_unfilled_point_is_an_error_naming_the_alternatives() {
    let cat = require!();
    let (codes, messages) = check(
        &cat,
        &product(r#"embedded(id = "dev")"#, "dev", &format!("{AUTHN})")),
    );
    assert!(
        codes.contains(&DiagnosticCode::PluginPointUnfilled),
        "{codes:?} {messages}"
    );
    assert!(
        messages.contains("static-authn-plugin") && messages.contains("oidc-authn-plugin"),
        "the message must list what could fill it: {messages}"
    );
}

#[test]
fn a_vendor_mismatch_is_an_error_naming_both_sides() {
    // The motivating failure: both sides read `vendor` from their own config, so
    // overriding one and not the other leaves the host resolving nothing --
    // silently, at runtime.
    let cat = require!();
    let (codes, messages) = check(
        &cat,
        &product(
            r#"embedded(id = "dev")"#,
            "dev",
            &format!(
                r#"{AUTHN}, plugins = [plugin("static-authn-plugin", config = {{"vendor": "acme"}})])"#
            ),
        ),
    );
    assert!(
        codes.contains(&DiagnosticCode::PluginVendorMismatch),
        "{codes:?} {messages}"
    );
    assert!(
        messages.contains("constructorfabric") && messages.contains("acme"),
        "the message must name what the host wants and what the plugin offers: {messages}"
    );
}

#[test]
fn overriding_the_vendor_on_both_sides_is_clean() {
    // The refusal must be surgical: matching overrides are legitimate.
    let cat = require!();
    let (codes, messages) = check(
        &cat,
        &product(
            r#"embedded(id = "dev")"#,
            "dev",
            &format!(
                r#"{AUTHN}, config = {{"vendor": "acme"}},
                   plugins = [plugin("static-authn-plugin", config = {{"vendor": "acme"}})])"#
            ),
        ),
    );
    assert!(codes.is_empty(), "{codes:?} {messages}");
}

// ---------------------------------------------------------------- profiles

const TWO_PROFILES: &str = r#"embedded(id = "dev"), kubernetes(id = "prod", discovery = "static")"#;

#[test]
fn different_plugins_in_different_profiles_do_not_collide() {
    // The canonical case, and the reason `plugin(...)` takes `profiles`.
    let cat = require!();
    let (codes, messages) = check(
        &cat,
        &product(
            TWO_PROFILES,
            "dev",
            &format!(
                r#"{AUTHN}, plugins = [
                     plugin("static-authn-plugin", profiles = ["dev"]),
                     plugin("oidc-authn-plugin", profiles = ["prod"]),
                   ])"#
            ),
        ),
    );
    assert!(codes.is_empty(), "{codes:?} {messages}");
}

#[test]
fn a_point_unfilled_in_only_one_profile_is_still_an_error() {
    // Scoping to dev leaves prod empty. Checking once, globally, would miss it.
    let cat = require!();
    let (codes, messages) = check(
        &cat,
        &product(
            TWO_PROFILES,
            "dev",
            &format!(r#"{AUTHN}, plugins = [plugin("static-authn-plugin", profiles = ["dev"])])"#),
        ),
    );
    assert!(
        codes.contains(&DiagnosticCode::PluginPointUnfilled),
        "{codes:?} {messages}"
    );
    assert!(
        messages.contains("profile `prod`"),
        "the message must name the profile that is short: {messages}"
    );
}

#[test]
fn the_same_plugin_twice_in_one_profile_collides() {
    // No catalogue needed, and so no skip guard: whether one implementation is
    // named twice for one host in one profile is a question the product file
    // answers on its own.
    let identity = FileIdentity {
        uri: "file:///t/product.gdl".to_owned(),
        source: SourceId::new("product").unwrap(),
        gdl_path: RelPath::new("product.gdl").unwrap(),
        load_paths: None,
    };
    let src = product(
        r#"embedded(id = "dev")"#,
        "dev",
        &format!(
            r#"{AUTHN}, plugins = [
                 plugin("static-authn-plugin"),
                 plugin("static-authn-plugin"),
               ])"#
        ),
    );
    let outcome = GdlEngine::new().eval_product(&identity, &src);
    let codes: Vec<DiagnosticCode> = outcome.diagnostics.iter().map(|d| d.code).collect();
    assert!(
        codes.contains(&DiagnosticCode::GdlDuplicateProfileScoped),
        "the product file can answer this one on its own: {codes:?}"
    );
}

// ---------------------------------------------------------------- ambiguity

#[test]
fn a_priority_tie_is_reported_as_undefined_not_decided() {
    // static-authn and oidc-authn both default to priority 100. The host takes
    // the lowest priority from whatever types-registry returns, and nothing
    // orders equal priorities -- so naming a winner would claim more than the
    // runtime guarantees.
    let cat = require!();
    let (codes, messages) = check(
        &cat,
        &product(
            r#"embedded(id = "dev")"#,
            "dev",
            &format!(
                r#"{AUTHN}, plugins = [
                     plugin("static-authn-plugin"), plugin("oidc-authn-plugin")
                   ])"#
            ),
        ),
    );
    assert!(
        codes.contains(&DiagnosticCode::PluginVendorAmbiguous),
        "{codes:?} {messages}"
    );
    assert!(
        messages.contains("undefined"),
        "a tie must be reported as undefined: {messages}"
    );
}

#[test]
fn distinct_priorities_do_decide_a_winner() {
    // tenant-resolver's three plugins default to 50 / 100 / 1000, so this one is
    // genuinely determined and the report should say so.
    let cat = require!();
    let (codes, messages) = check(
        &cat,
        &product(
            r#"embedded(id = "dev")"#,
            "dev",
            r#"use_gear("tenant-resolver", source = "gears-rust", plugins = [
                 plugin("static-tr-plugin"), plugin("rg-tr-plugin")
               ])"#,
        ),
    );
    assert!(
        codes.contains(&DiagnosticCode::PluginVendorAmbiguous),
        "{codes:?} {messages}"
    );
    assert!(
        messages.contains("`rg-tr-plugin` wins on priority") && !messages.contains("undefined"),
        "priority 50 beats 100, and that is determined: {messages}"
    );
}

// ---------------------------------------------------------------- orphans

#[test]
fn a_plugin_without_its_host_is_reported() {
    let cat = require!();
    let (codes, messages) = check(
        &cat,
        &product(
            r#"embedded(id = "dev")"#,
            "dev",
            r#"use_gear("static-authn-plugin", source = "gears-rust")"#,
        ),
    );
    assert!(
        codes.contains(&DiagnosticCode::PluginHostNotSelected),
        "{codes:?} {messages}"
    );
}

#[test]
fn a_gear_with_no_extension_points_is_left_alone() {
    let cat = require!();
    let (codes, messages) = check(
        &cat,
        &product(
            r#"embedded(id = "dev")"#,
            "dev",
            r#"use_gear("api-gateway", source = "gears-rust")"#,
        ),
    );
    assert!(codes.is_empty(), "{codes:?} {messages}");
}

/// The gap `PluginHostNotSelected` leaves, and the whole reason GBX0518 exists.
///
/// A plugin listed under a host that does not declare its point passed every
/// check while meaning nothing: `report_orphan_plugins` asks whether *some*
/// selected gear expects the point, and here one does -- `authn-resolver` is in
/// the product. So the misplacement was invisible, and the Add Gear panel
/// offered it because nothing refused it.
#[test]
fn a_plugin_under_the_wrong_host_is_an_error_naming_both() {
    let cat = require!();
    let (codes, messages) = check(
        &cat,
        &product(
            r#"embedded(id = "dev")"#,
            "dev",
            &format!(
                "{AUTHN}, plugins = [plugin(\"static-authn-plugin\")]), \
                 use_gear(\"types-registry\", source = \"gears-rust\", \
                 plugins = [plugin(\"oidc-authn-plugin\")])"
            ),
        ),
    );
    assert!(
        codes.contains(&DiagnosticCode::PluginPointNotDeclared),
        "{codes:?} {messages}"
    );
    assert!(
        messages.contains("types-registry") && messages.contains("oidc-authn-plugin"),
        "the message must name the host and the plugin: {messages}"
    );
    assert!(
        messages.contains("declares no extension point"),
        "and say what the host does declare: {messages}"
    );
}

/// Host declares *some* points, just not the one this plugin implements.
///
/// The empty-host branch (`declares no extension point`) is covered above;
/// this is the other formatting arm -- naming what the host *does* declare.
#[test]
fn a_plugin_under_a_host_with_other_points_names_them() {
    let cat = require!();
    let (codes, messages) = check(
        &cat,
        &product(
            r#"embedded(id = "dev")"#,
            "dev",
            &format!(
                "{AUTHN}, plugins = [plugin(\"static-authn-plugin\")]), \
                 use_gear(\"tenant-resolver\", source = \"gears-rust\", \
                 plugins = [plugin(\"oidc-authn-plugin\")])"
            ),
        ),
    );
    assert!(
        codes.contains(&DiagnosticCode::PluginPointNotDeclared),
        "{codes:?} {messages}"
    );
    assert!(
        messages.contains("tenant-resolver") && messages.contains("oidc-authn-plugin"),
        "the message must name the host and the plugin: {messages}"
    );
    assert!(
        messages.contains("declares ") && !messages.contains("declares no extension point"),
        "name the points the host has, not the empty-host sentence: {messages}"
    );
}

/// The negative case, which is the one that makes the check worth having: a
/// plugin under the host that *does* declare its point is silent.
#[test]
fn a_plugin_under_its_own_host_is_clean() {
    let cat = require!();
    let (codes, messages) = check(
        &cat,
        &product(
            r#"embedded(id = "dev")"#,
            "dev",
            &format!(r#"{AUTHN}, plugins = [plugin("oidc-authn-plugin")])"#),
        ),
    );
    assert!(
        !codes.contains(&DiagnosticCode::PluginPointNotDeclared),
        "{codes:?} {messages}"
    );
}

// --------------------------------------------------------------------------
// Declared roles, checked at catalogue-load time -- a different phase than the
// sections above, which join a loaded catalogue against a product. Each test
// builds its own source root, because the diagnostics under test fire while
// the catalogue itself is built.

/// A throwaway source root, written from `(path, contents)` pairs.
struct Root(PathBuf);

impl Root {
    fn new(label: &str, files: &[(&str, String)]) -> Self {
        static NTH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gbx-plugins-{label}-{}-{}",
            std::process::id(),
            NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        drop(std::fs::remove_dir_all(&dir));
        for (path, contents) in files {
            let at = dir.join(path);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(at, contents).unwrap();
        }
        Self(dir)
    }

    fn load(&self) -> Catalogue {
        let source = SourceRoot::open(SourceId::new("demo").unwrap(), &self.0).unwrap();
        load_catalogue(&[source]).catalogue
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

fn manifest(name: &str, lib: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nname = \"{lib}\"\npath = \"src/lib.rs\"\n"
    )
}

/// A plugin spec as the corpus declares one.
fn spec(ident: &str, segment: &str) -> String {
    format!(
        "#[gts_type_schema(base = PluginV1, type_id = gts_id!(\"cf.toolkit.plugins.plugin.v1~{segment}\"), description = \"d\", properties = \"\")]\npub struct {ident};\n"
    )
}

fn gear_rs(id: &str, extra: &str) -> String {
    format!("#[toolkit::gear(name = \"{id}\", capabilities = [system])]\npub struct G;\n{extra}")
}

fn gear_gdl(crate_name: &str, lib: &str, body: &str) -> String {
    format!(
        "gear(\n    maturity = \"stable\",\n    name = \"{crate_name}\",\n    description = \"d\",\n    category = \"core-functionality\",\n    visibility = \"internal\",\n    package = cargo(crate_name = \"{crate_name}\", lib = \"{lib}\", path = \".\"),\n{body}\n)\n"
    )
}

const THING_SDK: &str =
    "sdk = cargo(crate_name = \"thing-sdk\", lib = \"thing_sdk\", path = \"../thing-sdk\"),";

/// The thing SDK, a host declaring its point, and whatever plugins a test adds.
fn family(plugins: &[(&str, &str, &str)]) -> Vec<(&'static str, String)> {
    let mut files: Vec<(&'static str, String)> = vec![
        ("thing-sdk/Cargo.toml", manifest("thing-sdk", "thing_sdk")),
        (
            "thing-sdk/src/lib.rs",
            format!(
                "pub trait ThingPluginClient: Send + Sync {{}}\n{}",
                spec("ThingSpecV1", "x.thing.plugin.v1~")
            ),
        ),
        ("host/Cargo.toml", manifest("host", "host")),
        ("host/src/lib.rs", gear_rs("host", "")),
        (
            "host/gear.gdl",
            gear_gdl(
                "host",
                "host",
                &format!(
                    "    {THING_SDK}\n    extension_points = [extension_point(\"x.thing.plugin.v1~\", trait = \"ThingPluginClient\")],"
                ),
            ),
        ),
    ];
    for (dir, implements, body) in plugins {
        let leaked: &'static str = Box::leak(format!("{dir}/Cargo.toml").into_boxed_str());
        files.push((leaked, manifest(dir, &dir.replace('-', "_"))));
        let leaked: &'static str = Box::leak(format!("{dir}/src/lib.rs").into_boxed_str());
        files.push((leaked, gear_rs(dir, body)));
        let leaked: &'static str = Box::leak(format!("{dir}/gear.gdl").into_boxed_str());
        files.push((
            leaked,
            gear_gdl(
                dir,
                &dir.replace('-', "_"),
                &format!("    implements = \"{implements}\","),
            ),
        ));
    }
    files
}

fn codes(catalogue: &Catalogue) -> Vec<DiagnosticCode> {
    catalogue.diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn a_declared_plugin_joins_its_host() {
    let root = Root::new(
        "join",
        &family(&[(
            "plug",
            "x.thing.plugin.v1~",
            "pub struct P; impl thing_sdk::ThingPluginClient for P {}",
        )]),
    );
    let catalogue = root.load();
    assert!(codes(&catalogue).is_empty(), "{:#?}", catalogue.diagnostics);

    let host = &catalogue.gears[&gearbox_ir::GearId::new("host").unwrap()];
    assert_eq!(host.extension_points.len(), 1);
    assert_eq!(
        host.extension_points[0].spec,
        "cf.toolkit.plugins.plugin.v1~x.thing.plugin.v1~"
    );
    assert_eq!(host.extension_points[0].trait_ident, "ThingPluginClient");

    let plug = &catalogue.gears[&gearbox_ir::GearId::new("plug").unwrap()];
    let fill = plug.implements.as_ref().expect("a plugin");
    assert_eq!(fill.point.as_ref(), Some(&host.extension_points[0]));
    assert!(plug.extension_points.is_empty());
    assert!(
        plug.gts_types.is_empty(),
        "a plugin declares no sdk, so the host's spec is attributed once"
    );
}

#[test]
fn a_host_that_implements_its_own_trait_is_still_a_host() {
    // The account-management shape: a forwarding proxy implements the host's
    // own plugin trait outside tests. Declared, it stays a host.
    let mut files = family(&[]);
    files.retain(|(path, _)| *path != "host/src/lib.rs");
    files.push((
        "host/src/lib.rs",
        gear_rs(
            "host",
            "pub struct Proxy; impl thing_sdk::ThingPluginClient for Proxy {}",
        ),
    ));
    let catalogue = Root::new("proxy", &files).load();
    let host = &catalogue.gears[&gearbox_ir::GearId::new("host").unwrap()];
    assert!(host.implements.is_none());
    assert_eq!(host.extension_points.len(), 1);
    assert_eq!(host.gts_types.len(), 1, "and its spec is its own");
}

#[test]
fn a_spec_the_sdk_does_not_declare_is_gbx0516() {
    let mut files = family(&[]);
    files.retain(|(path, _)| *path != "host/gear.gdl");
    files.push((
        "host/gear.gdl",
        gear_gdl(
            "host",
            "host",
            &format!(
                "    {THING_SDK}\n    extension_points = [extension_point(\"x.missing.plugin.v1~\", trait = \"ThingPluginClient\")],"
            ),
        ),
    ));
    let catalogue = Root::new("nospec", &files).load();
    let found = catalogue
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::PluginPointUndetermined)
        .expect("GBX0516");
    assert!(
        found.message.contains("x.missing.plugin.v1~")
            && found.message.contains("x.thing.plugin.v1~"),
        "names the spec and what the sdk does declare: {}",
        found.message
    );
    let host = &catalogue.gears[&gearbox_ir::GearId::new("host").unwrap()];
    assert!(
        host.extension_points.is_empty(),
        "an unchecked point is not recorded"
    );
}

#[test]
fn a_trait_the_sdk_does_not_declare_is_gbx0516() {
    let mut files = family(&[]);
    files.retain(|(path, _)| *path != "host/gear.gdl");
    files.push((
        "host/gear.gdl",
        gear_gdl(
            "host",
            "host",
            &format!(
                "    {THING_SDK}\n    extension_points = [extension_point(\"x.thing.plugin.v1~\", trait = \"NoSuchClient\")],"
            ),
        ),
    ));
    let catalogue = Root::new("notrait", &files).load();
    let found = catalogue
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::PluginPointUndetermined)
        .expect("GBX0516");
    assert!(found.message.contains("NoSuchClient"), "{}", found.message);
}

#[test]
fn a_plugin_filling_a_spec_nobody_declares_is_gbx0519() {
    let catalogue = Root::new(
        "stray",
        &family(&[(
            "stray",
            "x.nobody.plugin.v1~",
            "pub struct P; impl thing_sdk::ThingPluginClient for P {}",
        )]),
    )
    .load();
    assert!(
        codes(&catalogue).contains(&DiagnosticCode::PluginSpecUndeclared),
        "{:#?}",
        catalogue.diagnostics
    );
    let stray = &catalogue.gears[&gearbox_ir::GearId::new("stray").unwrap()];
    assert_eq!(
        stray.implements.as_ref().and_then(|f| f.point.as_ref()),
        None
    );
}

#[test]
fn a_plugin_implementing_nothing_of_its_point_warns_and_still_fills() {
    let catalogue = Root::new("lazy", &family(&[("lazy", "x.thing.plugin.v1~", "")])).load();
    let found = catalogue
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::PluginImplMissing)
        .expect("GBX0526");
    assert_eq!(found.severity, gearbox_ir::Severity::Warning);
    let lazy = &catalogue.gears[&gearbox_ir::GearId::new("lazy").unwrap()];
    assert!(
        lazy.implements
            .as_ref()
            .and_then(|f| f.point.as_ref())
            .is_some(),
        "the declaration is the role; the impl is only evidence"
    );
}

#[test]
fn two_points_over_one_trait_stay_distinct() {
    // The bss shape: the ledger's rate provider and bss-rate-provider's sources
    // both implement one trait from the ledger's SDK, and only their specs
    // differ. A plugin of the second must not land on the first.
    let mut files = family(&[(
        "source",
        "x.source.plugin.v1~",
        "pub struct P; impl thing_sdk::ThingPluginClient for P {}",
    )]);
    files.extend([
        ("relay-sdk/Cargo.toml", manifest("relay-sdk", "relay_sdk")),
        ("relay-sdk/src/lib.rs", spec("SourceSpecV1", "x.source.plugin.v1~")),
        ("relay/Cargo.toml", manifest("relay", "relay")),
        ("relay/src/lib.rs", gear_rs("relay", "")),
        (
            "relay/gear.gdl",
            gear_gdl(
                "relay",
                "relay",
                "    sdk = cargo(crate_name = \"relay-sdk\", lib = \"relay_sdk\", path = \"../relay-sdk\"),\n    extension_points = [extension_point(\"x.source.plugin.v1~\", trait = \"ThingPluginClient\", sdk = cargo(crate_name = \"thing-sdk\", lib = \"thing_sdk\", path = \"../thing-sdk\"))],",
            ),
        ),
    ]);
    let catalogue = Root::new("twopoints", &files).load();
    assert!(codes(&catalogue).is_empty(), "{:#?}", catalogue.diagnostics);

    let source = &catalogue.gears[&gearbox_ir::GearId::new("source").unwrap()];
    let point = source
        .implements
        .as_ref()
        .and_then(|f| f.point.as_ref())
        .expect("joined");
    assert_eq!(
        point.spec,
        "cf.toolkit.plugins.plugin.v1~x.source.plugin.v1~"
    );
    assert_eq!(
        point.sdk_lib, "thing_sdk",
        "the trait's crate, not the relay's own sdk"
    );
    let host = &catalogue.gears[&gearbox_ir::GearId::new("host").unwrap()];
    assert_eq!(
        catalogue
            .implementations_of(&host.extension_points[0])
            .len(),
        0
    );
}

#[test]
fn plugin_interface_is_gone() {
    let mut files = family(&[]);
    files.retain(|(path, _)| *path != "host/gear.gdl");
    files.push((
        "host/gear.gdl",
        gear_gdl(
            "host",
            "host",
            &format!("    {THING_SDK}\n    plugin_interface = \"ThingPluginClient\","),
        ),
    ));
    let catalogue = Root::new("iface", &files).load();
    assert!(
        codes(&catalogue).contains(&DiagnosticCode::GdlUnknownArgument),
        "{:#?}",
        catalogue.diagnostics
    );
}

/// The resolver reports an unfilled point for the profile it resolves, and only
/// that one.
///
/// **The check used to be `validate`-only**, and nothing a person building the
/// product uses validates: the Studio and `generate` both resolve. Measured in
/// the Studio -- "errors 0" beside a description `validate` refused, and a tree
/// generated for it. A plugin scoped to `prod` implements the point there and leaves
/// `dev` empty, which is the case that shows the check is per profile rather
/// than a copy of `validate`'s all-profiles answer.
#[test]
fn a_resolution_reports_an_unfilled_point_for_its_own_profile() {
    let cat = require!();
    let src = product(
        r#"embedded(id = "dev"), kubernetes(id = "prod", discovery = "static")"#,
        "dev",
        &format!(r#"{AUTHN}, plugins = [plugin("oidc-authn-plugin", profiles = ["prod"])])"#),
    );
    let identity = FileIdentity {
        uri: "file:///t/product.gdl".to_owned(),
        source: SourceId::new("product").unwrap(),
        gdl_path: RelPath::new("product.gdl").unwrap(),
        load_paths: None,
    };
    let intent = GdlEngine::new()
        .eval_product(&identity, &src)
        .value
        .expect("evaluates");

    let unfilled = |profile: &str| {
        let resolution = gearbox_engine::resolve::resolve(
            &cat,
            &intent,
            &gearbox_ir::ProfileId::new(profile).unwrap(),
        );
        resolution
            .diagnostics
            .iter()
            .filter(|d| d.code == DiagnosticCode::PluginPointUnfilled)
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
    };

    let dev = unfilled("dev");
    assert_eq!(dev.len(), 1, "dev has no implementation linked: {dev:?}");
    assert!(dev[0].contains("in profile `dev`"), "{dev:?}");
    assert!(
        unfilled("prod").is_empty(),
        "prod links oidc-authn-plugin, and dev's gap is not prod's to report"
    );
}

const AM: &str = r#"use_gear("account-management", source = "gears-rust""#;

#[test]
fn a_host_is_matched_by_the_vendor_field_its_point_names() {
    // account-management has two `vendor` fields: `tr_plugin.vendor`
    // ("constructorfabric"), what it registers under as a tenant-resolver
    // plugin, and `idp.vendor` ("cf"), what it selects its IdP plugin by. The
    // first was read as the selector, so every product with the host failed
    // GBX0512 against `static-idp-plugin` (vendor "cf") -- which the runtime
    // finds. Its point now says `selector = "idp.vendor"`.
    let cat = require!();
    let am = cat
        .gear(&gearbox_ir::GearId::new("account-management").unwrap())
        .unwrap();
    assert_eq!(am.vendor_selector.as_deref(), Some("cf"));

    let (codes, messages) = check(
        &cat,
        &product(
            r#"embedded(id = "dev")"#,
            "dev",
            &format!(r#"{AM}, plugins = [plugin("static-idp-plugin")])"#),
        ),
    );
    assert!(codes.is_empty(), "{codes:?} {messages}");

    // And the product overrides it at the same path, not at a top-level `vendor`.
    let (codes, messages) = check(
        &cat,
        &product(
            r#"embedded(id = "dev")"#,
            "dev",
            &format!(
                r#"{AM}, config = {{"idp": {{"vendor": "keycloak"}}}}, plugins = [plugin("static-idp-plugin")])"#
            ),
        ),
    );
    assert!(
        codes.contains(&DiagnosticCode::PluginVendorMismatch) && messages.contains("keycloak"),
        "{codes:?} {messages}"
    );
}
