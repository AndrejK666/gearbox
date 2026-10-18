//! Cluster projection end to end, against the real `gears-rust` tree.
//!
//! The point of these is that the `cluster` gear's `gear.gdl` declares no
//! provider, no name and no capability -- only where the two plugin crates are.
//! Everything asserted below therefore came out of Rust, which is what ADR
//! `cpt-gearbox-adr-macro-projected-catalogue` requires.
//!
//! Skipped when the sibling checkout is absent, so the suite still runs
//! standalone.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::PathBuf;

use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::{Catalogue, ClusterPrimitive, GearId, SourceId};

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

macro_rules! require_tree {
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

fn cluster_gear(catalogue: &Catalogue) -> &gearbox_ir::GearDescriptor {
    catalogue
        .gears
        .get(&GearId::new("cluster").unwrap())
        .expect("the slice includes the cluster gear")
}

#[test]
fn provider_names_are_projected_not_declared() {
    let catalogue = require_tree!();
    let names: Vec<&str> = cluster_gear(&catalogue)
        .cluster_providers
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["k8s", "postgres", "redis", "standalone"],
        "names come from each plugin's `PROVIDER_NAME`; the description names \
         none of them -- it only says where each crate is"
    );
}

#[test]
fn the_capability_matrix_comes_out_of_rust() {
    let catalogue = require_tree!();
    let gear = cluster_gear(&catalogue);

    let caps = |name: &str, primitive: ClusterPrimitive| -> Vec<String> {
        let mut v: Vec<String> = gear
            .cluster_providers
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no provider named `{name}`"))
            .capabilities_for(primitive)
            .into_iter()
            .map(|c| c.as_str().to_owned())
            .collect();
        v.sort();
        v
    };

    assert_eq!(
        caps("standalone", ClusterPrimitive::Cache),
        vec![
            "cluster.cache.linearizable",
            "cluster.cache.prefix-watch",
            "cluster.cache.watch"
        ],
        "an in-process store watches a prefix natively, and a key as well"
    );
    assert_eq!(
        caps("postgres", ClusterPrimitive::Cache),
        vec!["cluster.cache.linearizable", "cluster.cache.watch"],
        "the NOTIFY channel carries one key per payload, so prefix routing is \
         infeasible -- and that absence is what later makes \
         cache(linearizable + prefix_watch) unsatisfiable in a multi-application \
         profile. One key it does watch, which is what `cluster.cache.watch` is \
         for: the two are separate questions and postgres answers them differently"
    );
    assert_eq!(
        caps("postgres", ClusterPrimitive::Lock),
        vec!["cluster.lock.linearizable"]
    );
}

/// **This claim asserted the opposite, and the corpus had already falsified
/// it.** While nothing registered leader election, "it always falls through to
/// the SDK compare-and-swap default" was worth pinning, and the sentence went
/// on to name the risk: *a provider claiming it would let the resolver bless a
/// binding the runtime cannot make.* `K8sLeaderElectionProvider` arrived on
/// 2026-09-02 and is exactly such a provider -- it is linked only under a cargo
/// feature.
///
/// So the claim becomes the one the risk implies: the registration is in the
/// catalogue, and it is recorded as conditional. What keeps the resolver from
/// blessing it is `a_gated_primitive_is_not_a_candidate_without_its_feature`
/// below.
#[test]
fn leader_election_is_registered_only_under_a_feature() {
    let catalogue = require_tree!();
    let gear = cluster_gear(&catalogue);
    let leader: Vec<&gearbox_ir::ClusterProviderDecl> = gear
        .cluster_providers
        .iter()
        .filter(|p| p.primitives.contains(&ClusterPrimitive::LeaderElection))
        .collect();
    assert_eq!(
        leader.len(),
        1,
        "the k8s plugin is the only one that registers leader election, got {:?}",
        leader.iter().map(|p| &p.name).collect::<Vec<_>>()
    );
    assert_eq!(
        leader[0].gated_by.get(&ClusterPrimitive::LeaderElection),
        Some(&gearbox_ir::FeatureGate::Feature("k8s".to_owned())),
        "recorded as unconditional, it would promise a backend a default build \
         does not link -- and the failure is the silent one: a scope that looks \
         bound and elects a leader per replica"
    );
}

#[test]
fn deployment_semantics_are_declared_because_rust_does_not_state_them() {
    let catalogue = require_tree!();
    let gear = cluster_gear(&catalogue);
    let by = |name: &str| {
        gear.cluster_providers
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no provider named `{name}`"))
    };

    assert!(
        by("standalone").process_local,
        "an in-memory store coordinates nothing across applications; this is the \
         fact GBX0503 rests on"
    );
    assert!(!by("standalone").needs_credentials);
    assert!(!by("postgres").process_local);
    assert!(by("postgres").needs_credentials);
}

#[test]
fn the_slice_projects_cleanly() {
    let catalogue = require_tree!();
    // 8 original + tenant-resolver + 5 plugin gears, and then the four
    // `platform-host` members that had no description until now:
    // account-management, authz-resolver, resource-group, credstore -- and then
    // the rest of the corpus's implemented gears: five plugins of hosts already
    // described (credstore, account-management x2, authz-resolver x2), and the
    // license-resolver and usage-collector families whole, and eight plain
    // services (bss-ledger, bss-pricing, file-parser, file-storage,
    // github-mirror, simple-user-settings, nodes-registry, oagw), and the
    // awkward seven: bss-rate-provider and its two sources, chat-engine, and
    // mini-chat's host and two co-located plugins. Then settings-service, the
    // one crate the retired `gear.toml` files knew about and no `gear.gdl` did;
    // the other eleven of those have no code and are designs, kept apart. Then
    // four that arrived upstream: bss-approvals and graph-storage, the
    // clickhouse usage-collector plugin, and bss-products, which got its crate
    // and so left the designs (ten now).
    //
    // The count is pinned on purpose. It is not what this test is about -- the
    // error list below is -- but a corpus that grows or shrinks under the suite
    // changes what every other corpus test means, and finding that out here is
    // cheaper than reading it as a failure somewhere else.
    assert_eq!(catalogue.gears.len(), 49);
    assert_eq!(catalogue.designs.len(), 10);
    let errors: Vec<String> = catalogue
        .diagnostics
        .iter()
        .filter(|d| d.severity == gearbox_ir::Severity::Error)
        .map(|d| format!("{} {}", d.code.as_str(), d.message))
        .collect();
    assert!(
        errors.is_empty(),
        "the slice must project with no errors; got {errors:#?}"
    );
}

#[test]
fn the_cluster_description_restates_nothing_it_could_project() {
    let Some(root) = gears_rust() else {
        eprintln!("skipping: ../gears-rust not present");
        return;
    };
    let text = std::fs::read_to_string(root.join("gears/system/cluster/cluster/gear.gdl"))
        .expect("the cluster description exists");

    // Comments explain the projection at length, and the prose legitimately uses
    // words like "primitives", so strip comments and match *field assignments*
    // rather than bare substrings. An earlier version of this check matched the
    // description's own explanatory text.
    let code: String = text
        .lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");

    for projected in [
        "cluster_providers",
        "runtime_caps",
        "colocated_deps",
        "lifecycle",
        "client",
        "id",
    ] {
        assert!(
            !code.contains(&format!("{projected} =")),
            "`{projected}` is projected from Rust and must not be assigned in \
             the description"
        );
    }
    assert!(
        !code.contains("provider("),
        "`provider(...)` left the GDL surface when providers became projected"
    );
    assert!(
        code.contains("cluster_plugin("),
        "the description still has to say where the plugin crates are"
    );
}
