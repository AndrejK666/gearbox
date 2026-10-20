//! One description, every target (test case G13, Gearbox ADR-0026 section 6).
//!
//! A product resolves to the same gears and the same contract bindings under
//! every profile it declares -- local in `embedded`, local or remote elsewhere --
//! or it gets a target-capability error naming the profile. Placement, ports and
//! transports may differ; *what* is composed and *who calls whom* may not. No
//! target silently drops or moves a gear.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use gearbox_engine::{SourceRoot, load_catalogue, load_product};
use gearbox_ir::{
    BindingMechanism, DeploymentProfileDecl, GearId, ResolvedBindingMode, Severity, SourceId,
};

#[path = "common/corpus.rs"]
mod corpus_support;

/// The products in this repository that declare more than one profile. Between
/// them they cover all three kinds.
const PRODUCTS: &[&str] = &["payments-demo", "integration-demo"];

/// What has to agree across profiles.
#[derive(Debug, PartialEq, Eq)]
struct Intent {
    gears: BTreeSet<GearId>,
    /// `(consumer, contract, provider)`.
    bindings: BTreeSet<(String, String, String)>,
    /// `(requester, scope, primitive)`. The cluster edge is recorded as a
    /// binding only where the requester runs apart from `cluster` (G6), which
    /// is placement; the request itself is the same in every profile.
    cluster: BTreeSet<(String, String, String)>,
}

fn product_path(product: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../products")
        .join(product)
        .join("product.gdl")
}

#[test]
fn a_product_composes_the_same_gears_and_bindings_on_every_target() {
    let Some(root) = corpus_support::root() else {
        eprintln!("skipping: gears-rust not present");
        return;
    };
    let opened = vec![SourceRoot::open(SourceId::new("gears-rust").unwrap(), &root).unwrap()];
    let scan = load_catalogue(&opened);

    for product in PRODUCTS {
        let path = product_path(product);
        let intent = load_product(&path, None)
            .intent
            .expect("the product evaluates");
        // A plugin selected for some profiles only is the description choosing
        // a different gear per target on purpose, so it is not compared.
        let scoped: BTreeSet<&GearId> = intent
            .selected_gears
            .iter()
            .flat_map(|selection| &selection.plugins)
            .filter(|plugin| !plugin.profiles.is_empty())
            .map(|plugin| &plugin.gear)
            .collect();

        let mut per_profile: BTreeMap<String, Intent> = BTreeMap::new();
        let mut kinds = BTreeSet::new();
        for (profile, declaration) in &intent.profiles {
            kinds.insert(match declaration {
                DeploymentProfileDecl::Embedded { .. } => "embedded",
                DeploymentProfileDecl::SelfHosted { .. } => "self_hosted",
                DeploymentProfileDecl::Kubernetes { .. } => "kubernetes",
            });
            let resolution =
                gearbox_engine::resolve::resolve_at(&scan.catalogue, &intent, profile, Some(&path));
            let errors: Vec<String> = resolution
                .diagnostics
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .map(|d| format!("{} {}", d.code.as_str(), d.message))
                .collect();
            assert!(
                errors.is_empty(),
                "{product}/{profile}: a target that cannot carry the intent says so, and \
                 this product is expected to resolve everywhere:\n{}",
                errors.join("\n")
            );

            let gears: BTreeSet<GearId> = resolution
                .partition
                .applications
                .iter()
                .flat_map(|application| application.gears.iter().cloned())
                .filter(|gear| !scoped.contains(gear))
                .collect();
            let cluster: BTreeSet<(String, String, String)> = resolution
                .cluster
                .iter()
                .flat_map(|record| {
                    record.requesters.iter().map(|requester| {
                        (
                            requester.to_string(),
                            record.scope.clone(),
                            format!("{:?}", record.primitive),
                        )
                    })
                })
                .collect();
            // And the converse: every requester in a process without
            // `cluster` reaches it through a recorded remote binding. Without
            // this, deleting that derivation would leave the test green.
            for application in &resolution.partition.applications {
                if application.gears.iter().any(|g| g.as_str() == "cluster") {
                    continue;
                }
                for (requester, ..) in &cluster {
                    if !application.gears.iter().any(|g| g.as_str() == requester) {
                        continue;
                    }
                    assert!(
                        resolution.bindings.iter().any(|b| {
                            b.consumer.as_str() == requester
                                && b.consumer_application == application.name
                                && b.provider.as_str() == "cluster"
                                && b.mechanism == BindingMechanism::ProvidesClientWiring
                        }),
                        "{product}/{profile}: `{requester}` in `{}` runs apart from cluster \
                         with no remote binding to it",
                        application.name
                    );
                }
            }
            let mut bindings = BTreeSet::new();
            for binding in &resolution.bindings {
                if binding.mechanism == BindingMechanism::ProvidesClientWiring
                    && binding.provider.as_str() == "cluster"
                {
                    assert!(
                        cluster
                            .iter()
                            .any(|(requester, ..)| *requester == binding.consumer.as_str()),
                        "{product}/{profile}: a remote cluster binding for `{}`, which requests \
                         no cluster primitive",
                        binding.consumer
                    );
                    continue;
                }
                if binding.mode == ResolvedBindingMode::Remote {
                    assert!(
                        !matches!(declaration, DeploymentProfileDecl::Embedded { .. }),
                        "{product}/{profile}: an embedded profile has nothing to call remotely: \
                         {} -> {}",
                        binding.consumer,
                        binding.provider
                    );
                }
                bindings.insert((
                    binding.consumer.to_string(),
                    binding.contract.to_string(),
                    binding.provider.to_string(),
                ));
            }
            per_profile.insert(
                profile.to_string(),
                Intent {
                    gears,
                    bindings,
                    cluster,
                },
            );
        }

        let mut profiles = per_profile.iter();
        let (first, expected) = profiles.next().expect("a product declares a profile");
        for (profile, actual) in profiles {
            assert_eq!(
                actual, expected,
                "{product}: `{profile}` composes something other than `{first}`"
            );
        }
        if *product == "payments-demo" {
            assert_eq!(kinds.len(), 3, "payments-demo covers every profile kind");
        }
    }
}
