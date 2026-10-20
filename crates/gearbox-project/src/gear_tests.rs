//! The `#[toolkit::gear(...)]` argument list, at the parser.
//!
//! `GearArgs` had no direct test: everything that reads it came in through the
//! corpus, which exercises the arguments the corpus happens to use. These pin
//! the two that are easy to get wrong -- a `bool` argument, and the
//! fallthrough that decides what happens to one nobody has modelled yet.

use std::path::PathBuf;

use syn::parse_quote;

use super::{GearArgs, ProjectedGear, project_gear};
use crate::attribute::gear_attribute_sites;
use crate::scan::RustFile;

fn parse(args: &proc_macro2::TokenStream) -> ProjectedGear {
    let attr: syn::Attribute = parse_quote!(#[toolkit::gear(#args)]);
    attr.parse_args::<GearArgs>()
        .expect("the attribute parses")
        .0
}

fn rust_file(relative: &str, src: &str) -> RustFile {
    RustFile {
        path: PathBuf::from(relative),
        relative: PathBuf::from(relative),
        ast: syn::parse_file(src).expect("fixture parses"),
    }
}

/// Project the single gear a fixture tree declares.
fn project(files: &[RustFile]) -> ProjectedGear {
    let sites = gear_attribute_sites(files);
    assert_eq!(sites.len(), 1, "the fixture declares one gear");
    project_gear(&sites[0]).expect("the attribute parses")
}

#[test]
fn one_per_installation_is_read_rather_than_left_unmodelled() {
    let projected = parse(&parse_quote!(
        name = "service-discovery",
        capabilities = [grpc, system, rest],
        one_per_installation = true
    ));
    assert!(projected.one_per_installation);
    assert!(
        projected.unmodelled.is_empty(),
        "modelled, so it must not also be recorded as a gap: {:?}",
        projected.unmodelled
    );
}

#[test]
fn a_gear_that_says_nothing_is_not_one_per_installation() {
    // Absence is the ordinary answer, and it must not be confused with a
    // declared `false`.
    let projected = parse(&parse_quote!(name = "api-gateway", capabilities = [rest]));
    assert!(!projected.one_per_installation);

    let declared_false = parse(&parse_quote!(
        name = "api-gateway",
        one_per_installation = false
    ));
    assert!(!declared_false.one_per_installation);
}

#[test]
fn an_argument_this_parser_does_not_model_is_recorded_as_a_gap() {
    // The fallthrough's purpose, pinned. This used to be where the comment
    // said `unmodelled` was "written here and read by nobody, so a gap is
    // *recorded* rather than surfaced". It is surfaced now, as `GBX0608` from
    // `merge`, which is asserted where it is raised.
    let projected = parse(&parse_quote!(name = "x", something_new = "value"));
    assert_eq!(projected.unmodelled, ["something_new"]);
}

/// The nested list had the same hole and no surfacing at all: an unknown
/// `lifecycle(...)` key was consumed and thrown away, so the next lifecycle
/// argument to land in the macro would have been reported by nothing.
#[test]
fn an_unmodelled_lifecycle_key_is_recorded_as_a_gap() {
    let projected = parse(&parse_quote!(
        name = "x",
        lifecycle(entry = "run", restart_policy = "always", drain)
    ));
    assert_eq!(
        projected.unmodelled,
        ["lifecycle.restart_policy", "lifecycle.drain"],
        "both the `key = value` and the bare-flag form"
    );
    // The keys it does model are still read.
    let lifecycle = projected.lifecycle.expect("a lifecycle clause");
    assert_eq!(lifecycle.entry.as_deref(), Some("run"));
}

#[test]
fn the_lifecycle_keys_this_parser_models_are_not_recorded_as_gaps() {
    let projected = parse(&parse_quote!(
        name = "x",
        lifecycle(entry = "run", stop_timeout = "30s", await_ready)
    ));
    assert!(
        projected.unmodelled.is_empty(),
        "modelled, so not also a gap: {:?}",
        projected.unmodelled
    );
    let lifecycle = projected.lifecycle.expect("a lifecycle clause");
    assert_eq!(lifecycle.stop_timeout.as_deref(), Some("30s"));
    assert!(lifecycle.await_ready);
}

// -- conditionality --------------------------------------------------------

#[test]
fn a_cfg_beside_the_gear_attribute_makes_it_conditional() {
    let files = [rust_file(
        "gear.rs",
        r#"
        #[cfg(feature = "experimental")]
        #[toolkit::gear(name = "experimental")]
        pub struct ExperimentalGear;
        "#,
    )];
    assert!(project(&files).conditional);
}

#[test]
fn a_gear_with_no_cfg_anywhere_is_unconditional() {
    let files = [
        rust_file("lib.rs", "pub mod gear;"),
        rust_file(
            "gear.rs",
            r#"
            #[toolkit::gear(name = "plain")]
            pub struct PlainGear;
            "#,
        ),
    ];
    assert!(!project(&files).conditional);
}

/// The half nothing could see. `scan_crate` discovers files by walking
/// directories and never reads the `mod` declarations, so a gear behind
/// `#[cfg(feature = "x")] mod gear;` carries no attribute of its own and
/// projected as unconditional -- exactly the claim `conditional` exists to
/// avoid making.
#[test]
fn a_cfg_on_the_mod_declaration_makes_the_gear_conditional() {
    let files = [
        rust_file(
            "lib.rs",
            r#"
            #[cfg(feature = "experimental")]
            pub mod gear;
            "#,
        ),
        rust_file(
            "gear.rs",
            r#"
            #[toolkit::gear(name = "experimental")]
            pub struct ExperimentalGear;
            "#,
        ),
    ];
    assert!(
        project(&files).conditional,
        "the gating is on the `mod`, and the gear item carries no cfg at all"
    );
}

#[test]
fn a_cfg_anywhere_on_the_module_path_counts() {
    let files = [
        rust_file("lib.rs", "pub mod domain;"),
        rust_file(
            "domain/mod.rs",
            r"
            #[cfg(test)]
            pub mod gear;
            ",
        ),
        rust_file(
            "domain/gear.rs",
            r#"
            #[toolkit::gear(name = "deep")]
            pub struct DeepGear;
            "#,
        ),
    ];
    assert!(
        project(&files).conditional,
        "the gate is two levels up, and the chain has to be walked to see it"
    );
}

/// `#[toolkit::consumes]` above `#[toolkit::gear]` keys its `consumer_wiring` by
/// the declared gear name; below it, by the kebab-case of the struct.
#[test]
fn consumes_above_the_gear_attribute_reads_the_gear_name() {
    let above = project(&[rust_file(
        "src/gear.rs",
        r#"
        #[toolkit::consumes(contract = authz_resolver_sdk::AuthZResolverApi, from = "authz-resolver")]
        #[toolkit::gear(name = "credstore", capabilities = [system])]
        pub struct CredStoreGear;
        "#,
    )]);
    assert_eq!(above.consumer_wiring_key, "credstore");

    let below = project(&[rust_file(
        "src/gear.rs",
        r#"
        #[toolkit::gear(name = "credstore", capabilities = [system])]
        #[toolkit::consumes(contract = authz_resolver_sdk::AuthZResolverApi, from = "authz-resolver")]
        pub struct CredStoreGear;
        "#,
    )]);
    assert_eq!(below.consumer_wiring_key, "cred-store-gear");
}
