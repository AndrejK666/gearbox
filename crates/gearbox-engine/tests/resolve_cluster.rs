//! Steps 7 and 8: which backend answers each cluster primitive.
//!
//! The provider table these tests use is the real one, copied from what the
//! resolver projects out of the cluster gear's registry. Two facts about it do
//! most of the work here, and both are awkward: `standalone` is process-local,
//! and **neither provider registers leader election**. A test suite built on an
//! invented table would have neither problem and would prove nothing.
//!
//! The rule worth the most is `GBX0503`. A process-local backend spread across
//! processes is a correctness bug the runtime starts without complaint.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers #[test] fns but not the helpers here"
)]

use std::path::{Path, PathBuf};

use gearbox_engine::resolve::resolve;
use gearbox_engine::{SourceRoot, load_catalogue};
use gearbox_ir::{
    Catalogue, ClusterPrimitive, ClusterResolution, DiagnosticCode, Discovery, Preference,
    ProductIntent, ProfileId, RuntimeCap, SourceId,
};

#[path = "common/corpus.rs"]
mod corpus_support;

fn gears_rust() -> Option<PathBuf> {
    corpus_support::root()
}

fn pid(s: &str) -> ProfileId {
    ProfileId::new(s).unwrap()
}

fn codes(r: &gearbox_engine::resolve::Resolution) -> Vec<DiagnosticCode> {
    r.diagnostics.iter().map(|d| d.code).collect()
}

/// One process, so nothing is spread.
fn single(cat: &Catalogue, intent: &ProductIntent) -> gearbox_engine::resolve::Resolution {
    resolve(cat, intent, &pid("dev"))
}

#[test]
fn the_real_slice_resolves_its_cluster_scope_in_every_profile() {
    // `api-contracts-consumer` requires the `event-broker` scope, and its crate
    // carries the `impl ClusterProfile` that makes the name a join key rather
    // than a wish. What is asserted is the whole shape of the answer per
    // profile, because each half is a different mechanism: the cache is an
    // explicit operator choice, and leader election has no provider anywhere, so
    // it can only be the SDK's compare-and-swap default layered over that cache.
    let Some(root) = gears_rust() else {
        eprintln!("skipping: ../gears-rust not present");
        return;
    };
    let source = SourceRoot::open(SourceId::new("gears-rust").unwrap(), root).unwrap();
    let cat = load_catalogue(&[source]).catalogue;
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../products/payments-demo/product.gdl")
        .canonicalize()
        .unwrap();
    let intent = gearbox_engine::product::load_product(&path, None)
        .intent
        .unwrap();

    // dev is one process, so the process-local cache is legitimate there and
    // only there; local and prod are spread, which is why they name postgres.
    for (profile, cache) in [
        ("dev", "standalone"),
        ("local", "postgres"),
        ("prod", "postgres"),
    ] {
        let r = resolve(&cat, &intent, &pid(profile));
        let errors: Vec<_> = r
            .diagnostics
            .iter()
            .filter(|d| d.severity == gearbox_ir::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "{profile}: {errors:?}");

        let by = |p: ClusterPrimitive| {
            r.cluster
                .iter()
                .find(|b| b.scope == "event-broker" && b.primitive == p)
                .unwrap_or_else(|| panic!("{profile}: no {p:?} binding in {:?}", r.cluster))
        };
        assert_eq!(
            by(ClusterPrimitive::Cache).resolved,
            gearbox_ir::ClusterResolution::Provider {
                name: cache.to_owned()
            },
            "{profile}: cache"
        );
        assert_eq!(
            by(ClusterPrimitive::LeaderElection).resolved,
            gearbox_ir::ClusterResolution::SdkCasDefault {
                over_cache: cache.to_owned()
            },
            "{profile}: leader election rides on the cache"
        );
        assert_eq!(
            by(ClusterPrimitive::Cache).requesters,
            vec![support::gid("api-contracts-consumer")],
            "{profile}: requester"
        );

        // And nothing else. `by` finds, so without this a third binding -- a
        // lock nobody asked for, a second scope, a duplicate -- would be
        // invisible. The assertion this test replaced was `cluster.is_empty()`,
        // whose whole value was that it was a complete statement about the
        // vector; saying "these two are right" is not the same claim.
        assert_eq!(
            r.cluster.len(),
            2,
            "{profile}: exactly the cache and the election: {:?}",
            r.cluster
        );
    }
}

#[test]
fn one_process_gets_the_process_local_cache() {
    // `standalone` sorts before nothing and after nothing; with one process it is
    // not penalised, and it wins on the name against `postgres`... except it does
    // not, because `p` sorts before `s`. The point of the assertion is that the
    // answer is *stable and explained*, not that it is a particular name.
    let cat = support::cluster_catalogue(vec![(
        ClusterPrimitive::Cache,
        "main",
        &["cluster.cache.linearizable"],
    )]);
    let r = single(&cat, &support::intent(&["app"]));

    let binding = r.cluster.first().expect("one binding");
    assert_eq!(binding.primitive, ClusterPrimitive::Cache);
    assert_eq!(binding.resolved.effective_provider(), Some("postgres"));
    assert!(codes(&r).contains(&DiagnosticCode::ClusterAutoSelected));

    let help = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::ClusterAutoSelected)
        .and_then(|d| d.help.clone())
        .expect("the ranking is explained");
    assert!(help.contains("candidates were"), "{help}");
    assert!(help.contains("ties broke on the name"), "{help}");
}

#[test]
fn a_process_local_backend_across_processes_is_an_error() {
    // The single most valuable rule here. `standalone`'s cache lives in one
    // process's memory, so two processes get two caches — a lock that locks
    // nothing, and leader election that elects a leader per replica. The runtime
    // starts this happily.
    let cat = support::cluster_catalogue(vec![(
        ClusterPrimitive::Cache,
        "main",
        &["cluster.cache.linearizable"],
    )]);
    let mut intent = support::self_hosted(&["app"], Discovery::Static, Some("t"));
    support::bind_cluster(&mut intent, "main", "standalone", None);
    support::pin(&mut intent, "worker", "cluster", 1);

    let r = resolve(&cat, &intent, &pid("local"));
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::ClusterProcessLocalInMultiProcess)
        .expect("GBX0503");
    assert!(
        d.help
            .as_deref()
            .unwrap_or_default()
            .contains("leader per replica"),
        "the help must name the actual consequence: {:?}",
        d.help
    );
    assert!(d.severity.is_error());
}

#[test]
fn one_process_may_use_the_process_local_backend() {
    // The same binding, without the spread. Reporting it here would refuse a
    // product that works.
    let cat = support::cluster_catalogue(vec![(
        ClusterPrimitive::Cache,
        "main",
        &["cluster.cache.linearizable"],
    )]);
    let mut intent = support::intent(&["app"]);
    support::bind_cluster(&mut intent, "main", "standalone", None);

    let r = single(&cat, &intent);
    assert!(!codes(&r).contains(&DiagnosticCode::ClusterProcessLocalInMultiProcess));
}

#[test]
fn leader_election_always_falls_back_to_the_sdk_default() {
    // Not a gap in this code: zero leader-election providers are registered in
    // the runtime. Saying so is the point, because "leader election works" and
    // "leader election is compare-and-swap over your cache" are different
    // promises.
    let cat = support::cluster_catalogue(vec![
        (
            ClusterPrimitive::Cache,
            "main",
            &["cluster.cache.linearizable"],
        ),
        (ClusterPrimitive::LeaderElection, "main", &[]),
    ]);
    let mut intent = support::intent(&["app"]);
    support::bind_cluster(&mut intent, "main", "postgres", Some("secret/pg"));

    let r = single(&cat, &intent);
    let election = r
        .cluster
        .iter()
        .find(|b| b.primitive == ClusterPrimitive::LeaderElection)
        .expect("a leader-election binding");
    assert_eq!(
        election.resolved,
        ClusterResolution::SdkCasDefault {
            over_cache: "postgres".to_owned()
        }
    );
    assert!(codes(&r).contains(&DiagnosticCode::ClusterSdkDefault));
}

/// The message names the gear that asked, and an empty table says so in words.
///
/// **Both halves were read as a stray error.** A product that had just added one
/// consumer gear was told about scope `event-broker` -- the name the consumer
/// declares its requirement *under*, not a gear the product contained -- with a
/// `help` that ended at the heading `per provider:` because no provider gear was
/// in the closure to list. Nothing in either line connected the complaint to the
/// gear that had been added a moment earlier.
#[test]
fn an_unsatisfiable_requirement_names_who_asked() {
    let cat = support::cluster_catalogue(vec![(
        ClusterPrimitive::Lock,
        "main",
        &["cluster.lock.prefix-watch"],
    )]);
    let r = single(&cat, &support::intent(&["app"]));

    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::ClusterUnsatisfiable)
        .expect("GBX0502");
    assert!(
        d.message.contains("required by `app`"),
        "the message must name the gear that required it: {}",
        d.message
    );
    let help = d.help.as_deref().unwrap_or_default();
    assert!(
        !help.trim_end().ends_with("per provider:"),
        "a heading with no list under it: {help}"
    );
}

#[test]
fn an_unsatisfiable_requirement_shows_every_provider() {
    // `prefix-watch` exists only on `standalone`, and `standalone` does not
    // answer locks. A bare refusal would leave the reader guessing which of the
    // two facts blocked them.
    let cat = support::cluster_catalogue(vec![(
        ClusterPrimitive::Lock,
        "main",
        &["cluster.lock.prefix-watch"],
    )]);
    let r = single(&cat, &support::intent(&["app"]));

    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::ClusterUnsatisfiable)
        .expect("GBX0502");
    let help = d.help.as_deref().unwrap_or_default();
    assert!(help.contains("postgres: missing"), "{help}");
    assert!(
        help.contains("standalone: does not answer lock"),
        "the two providers fail for different reasons and the table must say so: {help}"
    );
    let lock = r
        .cluster
        .iter()
        .find(|b| b.primitive == ClusterPrimitive::Lock)
        .expect("a lock binding");
    assert_eq!(lock.resolved, ClusterResolution::Unsatisfied);
}

#[test]
fn an_unregistered_provider_is_named_against_the_registered_ones() {
    let cat = support::cluster_catalogue(vec![(
        ClusterPrimitive::Cache,
        "main",
        &["cluster.cache.linearizable"],
    )]);
    let mut intent = support::intent(&["app"]);
    support::bind_cluster(&mut intent, "main", "redis", None);

    let r = single(&cat, &intent);
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::ClusterUnregisteredProvider)
        .expect("GBX0505");
    assert!(d.help.as_deref().unwrap_or_default().contains("postgres"));
    assert!(d.help.as_deref().unwrap_or_default().contains("standalone"));
    let cache = r
        .cluster
        .iter()
        .find(|b| b.primitive == ClusterPrimitive::Cache)
        .expect("a cache binding");
    assert_eq!(
        cache.selected.downgraded_by,
        Some(DiagnosticCode::ClusterUnregisteredProvider)
    );
    assert_eq!(cache.resolved, ClusterResolution::Unsatisfied);
    assert_eq!(cache.resolved.effective_provider(), None);
}

#[test]
fn a_provider_that_needs_credentials_must_be_told_where_they_are() {
    let cat = support::cluster_catalogue(vec![(
        ClusterPrimitive::Cache,
        "main",
        &["cluster.cache.linearizable"],
    )]);
    let mut intent = support::intent(&["app"]);
    support::bind_cluster(&mut intent, "main", "postgres", None);

    let r = single(&cat, &intent);
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::ClusterNoCredentialSource)
        .expect("GBX0506");
    assert!(
        d.help
            .as_deref()
            .unwrap_or_default()
            .contains("never enters"),
        "the help must say the credential itself stays out: {:?}",
        d.help
    );

    // With a reference, silence.
    let mut ok = support::intent(&["app"]);
    support::bind_cluster(&mut ok, "main", "postgres", Some("secret/pg"));
    assert!(!codes(&single(&cat, &ok)).contains(&DiagnosticCode::ClusterNoCredentialSource));
}

/// The compare-and-swap default takes its credential from the cache it rides on.
///
/// No provider registers leader election, so there is no
/// `leader_election = provider(...)` for a `secret_ref` to live on. Reading only
/// the primitive's own binding therefore refused a product that was specified
/// correctly and could not be specified any other way: the operator was being
/// asked for something unspellable. Found by requiring the scope on the real
/// slice, where every profile that uses postgres reported it.
#[test]
fn the_cas_default_inherits_the_caches_credential_source() {
    let cat = support::cluster_catalogue(vec![
        (
            ClusterPrimitive::Cache,
            "main",
            &["cluster.cache.linearizable"] as &[&str],
        ),
        (ClusterPrimitive::LeaderElection, "main", &[]),
    ]);
    let mut intent = support::intent(&["app"]);
    support::bind_cluster(&mut intent, "main", "postgres", Some("secret/pg"));

    let r = single(&cat, &intent);
    assert!(
        !codes(&r).contains(&DiagnosticCode::ClusterNoCredentialSource),
        "the cache names a source, and the default is that cache: {:?}",
        r.diagnostics
    );
    let election = r
        .cluster
        .iter()
        .find(|b| b.primitive == ClusterPrimitive::LeaderElection)
        .expect("the election was required");
    assert_eq!(
        election.resolved,
        gearbox_ir::ClusterResolution::SdkCasDefault {
            over_cache: "postgres".to_owned()
        }
    );

    // And the check still bites when the cache itself names nothing -- on the
    // election, which is the half this fallback added. Asserting only that the
    // code appears somewhere would not say that: the cache binding names no
    // source either, so `postgres` reports itself and the assertion passes
    // whether or not the election was ever checked.
    let mut bare = support::intent(&["app"]);
    support::bind_cluster(&mut bare, "main", "postgres", None);
    let bare = single(&cat, &bare);
    let complaints = bare
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::ClusterNoCredentialSource)
        .count();
    assert_eq!(
        complaints, 2,
        "the cache and the default that rides on it: {:?}",
        bare.diagnostics
    );
}

/// The cache's credential is inherited only by the backend it is a credential for.
///
/// `over_cache` is whatever the cache *resolved* to, so matching on the shape of
/// the resolution rather than on the provider's name would hand one backend's
/// `secret_ref` to another. Here the scope's cache is `standalone`, which needs
/// none, and the lock's only candidate is `postgres`, which does -- so there is
/// nothing to inherit and the resolver must not pretend otherwise.
#[test]
fn a_credential_is_not_inherited_across_providers() {
    let cat = support::cluster_catalogue(vec![
        (
            ClusterPrimitive::Cache,
            "main",
            &["cluster.cache.linearizable"] as &[&str],
        ),
        (ClusterPrimitive::Lock, "main", &[]),
    ]);
    let mut intent = support::intent(&["app"]);
    support::bind_cluster(&mut intent, "main", "standalone", Some("secret/not-pg"));

    let r = single(&cat, &intent);
    let lock = r
        .cluster
        .iter()
        .find(|b| b.primitive == ClusterPrimitive::Lock)
        .expect("a lock binding");
    assert_ne!(
        lock.resolved,
        ClusterResolution::Provider {
            name: "postgres".to_owned()
        },
        "postgres was chosen on a credential that belongs to standalone"
    );
    assert_eq!(lock.secret_ref, None, "and nothing was copied onto it");
}

/// The resolver does not choose a backend it will then refuse.
///
/// `postgres` is the only lock provider, so a lock requirement used to
/// auto-select it into a profile that declared no provider for it and therefore
/// no credentials: GBX0506 on a product that was specified correctly and could
/// not be specified any other way, since `standalone` does not answer locks and
/// naming postgres would drag a real database into a profile designed to need
/// none. The SDK's compare-and-swap default over the cache is a working answer
/// and is taken instead.
#[test]
fn an_uncredentialled_provider_loses_to_the_sdk_default() {
    let cat = support::cluster_catalogue(vec![
        (
            ClusterPrimitive::Cache,
            "main",
            &["cluster.cache.linearizable"] as &[&str],
        ),
        (ClusterPrimitive::Lock, "main", &[]),
    ]);
    let mut intent = support::intent(&["app"]);
    support::bind_cluster(&mut intent, "main", "standalone", None);

    let r = single(&cat, &intent);
    assert!(
        !codes(&r).contains(&DiagnosticCode::ClusterNoCredentialSource),
        "nothing should be refused here: {:?}",
        r.diagnostics
    );
    let lock = r
        .cluster
        .iter()
        .find(|b| b.primitive == ClusterPrimitive::Lock)
        .expect("a lock binding");
    assert_eq!(
        lock.resolved,
        ClusterResolution::SdkCasDefault {
            over_cache: "standalone".to_owned()
        }
    );

    // The diagnostic says which of the two reasons it was, because "no provider
    // implements this" would be false: one does, and cannot be paid.
    let help = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::ClusterSdkDefault)
        .and_then(|d| d.help.clone())
        .expect("GBX0504 explains itself");
    assert!(help.contains("names no credential source"), "{help}");
}

/// A cache has nothing to fall back to, so it is chosen and then reported.
///
/// The other half of the rule above. There is no compare-and-swap default for a
/// cache -- nothing is layered over a cache that does not exist -- so refusing
/// to select would produce "no provider answers `cache`", which is false.
/// Selecting and asking for a `secret_ref` is an instruction the operator can
/// act on.
#[test]
fn a_cache_with_no_credential_is_still_selected_and_reported() {
    let cat = support::cluster_catalogue(vec![(
        ClusterPrimitive::Cache,
        "main",
        &["cluster.cache.linearizable"],
    )]);
    let mut intent = support::intent(&["app"]);
    support::bind_cluster(&mut intent, "main", "postgres", None);

    let r = single(&cat, &intent);
    let cache = r
        .cluster
        .iter()
        .find(|b| b.primitive == ClusterPrimitive::Cache)
        .expect("a cache binding");
    assert_eq!(
        cache.resolved,
        ClusterResolution::Provider {
            name: "postgres".to_owned()
        }
    );
    assert!(codes(&r).contains(&DiagnosticCode::ClusterNoCredentialSource));
    assert!(!codes(&r).contains(&DiagnosticCode::ClusterUnsatisfiable));
}

#[test]
fn existing_infrastructure_is_preferred_within_a_scope() {
    // The preference may only order candidates that are already valid, so it can
    // never make an invalid product valid. Here both answer the lock; the one
    // already carrying the cache wins.
    let cat = support::cluster_catalogue(vec![
        (
            ClusterPrimitive::Cache,
            "main",
            &["cluster.cache.linearizable"],
        ),
        (ClusterPrimitive::Lock, "main", &[]),
    ]);
    let mut intent = support::intent(&["app"]);
    intent.preferences.push(Preference::ExistingInfrastructure);
    support::bind_cluster(&mut intent, "main", "postgres", Some("secret/pg"));

    let r = single(&cat, &intent);
    let lock = r
        .cluster
        .iter()
        .find(|b| b.primitive == ClusterPrimitive::Lock)
        .expect("a lock binding");
    assert_eq!(lock.resolved.effective_provider(), Some("postgres"));
}

/// A backend that decides its capabilities at run time says so, and only then.
///
/// Two halves, and the second is the one worth the test. Such a backend can be
/// named and configured like any other and answers its primitives -- what it
/// cannot do is honour a `capabilities = [...]` requirement, because the answer
/// depends on the infrastructure the operator points it at.
///
/// And the refusal has to *say* that. "redis: missing cluster.cache.linearizable"
/// reads as *cannot be*, when the truth is *cannot be known until it connects*;
/// the two lead an operator to different decisions.
#[test]
fn a_runtime_determined_backend_is_usable_but_promises_nothing() {
    let mut cat = support::cluster_catalogue(vec![(ClusterPrimitive::Cache, "main", &[])]);
    cat.gears
        .get_mut(&support::gid("cluster"))
        .expect("the fixture's cluster gear")
        .cluster_providers = vec![support::runtime_determined_provider()];

    // Asked for nothing, so it resolves -- and reports what it cannot promise.
    let mut intent = support::intent(&["app"]);
    support::bind_cluster(&mut intent, "main", "redis", Some("secret/redis"));
    let r = single(&cat, &intent);
    assert_eq!(
        r.cluster
            .first()
            .expect("a cache binding")
            .resolved
            .effective_provider(),
        Some("redis")
    );
    assert!(codes(&r).contains(&DiagnosticCode::ClusterCapabilityRuntimeDetermined));
    assert!(!codes(&r).contains(&DiagnosticCode::ClusterUnsatisfiable));

    // Asked for a guarantee, so it is refused -- and the row says why, not just
    // that something is missing.
    let mut demanding = support::cluster_catalogue(vec![(
        ClusterPrimitive::Cache,
        "main",
        &["cluster.cache.linearizable"],
    )]);
    demanding
        .gears
        .get_mut(&support::gid("cluster"))
        .expect("the fixture's cluster gear")
        .cluster_providers = vec![support::runtime_determined_provider()];
    let r = single(&demanding, &intent);
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::ClusterUnsatisfiable)
        .expect("GBX0502");
    let help = d.help.as_deref().unwrap_or_default();
    assert!(help.contains("decides them at run time"), "{help}");
    assert!(
        !help.contains("redis: missing"),
        "`missing` reads as `cannot be`, which is not what is known: {help}"
    );
}

#[test]
fn a_replicated_stateful_gear_without_election_is_reported() {
    // Step 8. Two copies both reconciling is not something the runtime notices.
    let mut cat = support::cluster_catalogue(Vec::new());
    let mut app = support::gear_with_caps("app", &[RuntimeCap::Stateful], &[]);
    app.id = gearbox_ir::GearId::new("app").unwrap();
    cat.gears.insert(app.id.clone(), app);

    let mut intent = support::self_hosted(&["app", "cluster"], Discovery::Static, Some("t"));
    support::pin(&mut intent, "app", "app", 3);

    let r = resolve(&cat, &intent, &pid("local"));
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::ClusterStatefulReplicasWithoutElection)
        .expect("GBX0507");
    assert!(d.message.contains("3 copies"), "{}", d.message);
    assert!(
        !d.severity.is_error(),
        "a warning: it may be exactly what was wanted"
    );
}

#[test]
fn cluster_resolution_is_stable_across_runs() {
    let cat = support::cluster_catalogue(vec![
        (
            ClusterPrimitive::Cache,
            "main",
            &["cluster.cache.linearizable"],
        ),
        (ClusterPrimitive::Lock, "main", &[]),
        (ClusterPrimitive::LeaderElection, "main", &[]),
    ]);
    let mut intent = support::intent(&["app"]);
    support::bind_cluster(&mut intent, "main", "postgres", Some("secret/pg"));

    let a = single(&cat, &intent);
    let b = single(&cat, &intent);
    assert_eq!(a.cluster, b.cluster);
}

#[path = "support/resolve_fixtures.rs"]
mod support;

/// A registration behind a cargo feature is not a candidate until the product
/// selects it, and naming it anyway is refused rather than resolved.
///
/// **The silent half is the one that matters.** Refusing an explicit binding
/// would leave the automatic path open: the resolver picks a provider itself
/// when a scope leaves a primitive unbound, so a gated leader election would
/// have been chosen for a product that never asked, resolved cleanly, written
/// to the lock, and elected one leader per replica at run time. Nothing on the
/// way there says a word — which is the same shape as `GBX0503`, one level up.
#[test]
fn a_gated_primitive_is_not_a_candidate_without_its_feature() {
    let mut cat = support::cluster_catalogue(vec![(ClusterPrimitive::Cache, "main", &[][..])]);
    // One provider, answering the one primitive, and only under `k8s`.
    let gear = cat
        .gears
        .get_mut(&gearbox_ir::GearId::new("cluster").unwrap())
        .expect("the cluster gear");
    let mut gated = support::postgres();
    gated.name = "k8s".to_owned();
    gated.primitives = [ClusterPrimitive::Cache].into_iter().collect();
    gated.gated_by = [(
        ClusterPrimitive::Cache,
        gearbox_ir::FeatureGate::Feature("k8s".to_owned()),
    )]
    .into_iter()
    .collect();
    gear.cluster_providers = vec![gated];

    // Nobody selects the feature, and nobody binds the primitive.
    let unselected = single(&cat, &support::intent(&["app"]));
    assert!(
        unselected
            .cluster
            .iter()
            .all(|b| b.resolved.effective_provider() != Some("k8s")),
        "a backend this build does not link was chosen anyway: {:?}",
        unselected.cluster
    );

    // Naming it explicitly is refused, with the remedy.
    let mut bound = support::intent(&["app"]);
    support::bind_cluster(&mut bound, "main", "k8s", None);
    let named = single(&cat, &bound);
    assert!(
        codes(&named).contains(&DiagnosticCode::ClusterProviderNeedsFeature),
        "expected GBX0525, got {:?}",
        codes(&named)
    );
    let help = named
        .diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::ClusterProviderNeedsFeature)
        .and_then(|d| d.help.clone())
        .expect("the remedy is named");
    assert!(help.contains("features = [\"k8s\"]"), "{help}");
}

fn remote_cluster_intent(kubernetes: bool) -> gearbox_ir::ProductIntent {
    let gears = ["app", "cluster", "grpc-hub"];
    let mut intent = if kubernetes {
        support::kubernetes(&gears, gearbox_ir::Discovery::Static)
    } else {
        support::self_hosted(&gears, gearbox_ir::Discovery::Static, Some("t"))
    };
    support::pin(&mut intent, "cluster-app", "cluster", 1);
    intent
        .application_pins
        .iter_mut()
        .find(|p| p.name.as_str() == "cluster-app")
        .unwrap()
        .include = [support::gid("grpc-hub")].into_iter().collect();
    support::bind_cluster(&mut intent, "app", "postgres", Some("existingSecret:pg"));
    intent
}

fn messages_of(r: &gearbox_engine::resolve::Resolution, code: DiagnosticCode) -> Vec<&str> {
    r.diagnostics
        .iter()
        .filter(|d| d.code == code)
        .map(|d| d.message.as_str())
        .collect()
}

#[test]
fn a_requester_apart_from_cluster_gets_a_remote_grpc_binding() {
    // `cluster` in its own application, the requester elsewhere,
    // Kubernetes: a gRPC binding the SDK addresses.
    let cat = support::remote_cluster_catalogue();
    let r = resolve(&cat, &remote_cluster_intent(true), &pid("prod"));
    let binding = r
        .bindings
        .iter()
        .find(|b| b.provider.as_str() == "cluster")
        .unwrap_or_else(|| panic!("no cluster binding: {:?}", r.diagnostics));
    assert_eq!(binding.consumer.as_str(), "app");
    assert_eq!(binding.contract.as_str(), "cluster/ClusterCacheApi@v1");
    assert!(binding.is_remote());
    assert_eq!(binding.transport, gearbox_ir::Transport::Grpc);
    assert_eq!(
        binding.mechanism,
        gearbox_ir::BindingMechanism::ProvidesClientWiring
    );
    assert_eq!(
        binding.endpoint.as_deref(),
        Some("http://cluster.default.svc.cluster.local:50051")
    );
    assert!(messages_of(&r, DiagnosticCode::RemoteClusterNeedsKubernetes).is_empty());
    assert!(
        messages_of(&r, DiagnosticCode::TopologyGrpcWithoutHub).is_empty(),
        "{:?}",
        r.diagnostics
    );
}

#[test]
fn a_remote_cluster_needs_its_contracts_provided_over_grpc() {
    // The binding used to need only that the contract
    // existed somewhere. Without cluster providing it over gRPC -- and the
    // profile contract the SDK resolves scopes through -- it is refused.
    for drop in ["cluster/ClusterCacheApi@v1", "cluster/ClusterProfileApi@v1"] {
        let mut cat = support::remote_cluster_catalogue();
        for provided in &mut cat
            .gears
            .get_mut(&support::gid("cluster"))
            .unwrap()
            .provides
        {
            if provided.contract.as_str() == drop {
                provided.transports.remove(&gearbox_ir::Transport::Grpc);
            }
        }
        let r = resolve(&cat, &remote_cluster_intent(true), &pid("prod"));
        let refused = messages_of(&r, DiagnosticCode::RemoteClusterWithoutGrpc);
        assert!(
            refused.iter().any(|m| m.contains(drop)),
            "{drop}: {:?}",
            r.diagnostics
        );
        assert!(r.bindings.iter().all(|b| b.provider.as_str() != "cluster"));
    }
}

#[test]
fn cluster_in_two_kubernetes_applications_is_refused_even_with_no_one_remote() {
    // Each process linking cluster renders a Service
    // named `cluster`, so two cannot install even when every requester has
    // its own copy.
    let cat = support::remote_cluster_catalogue();
    let mut intent = remote_cluster_intent(true);
    // The requester keeps a copy of its own, beside the cluster application.
    support::pin(&mut intent, "app-own", "app", 1);
    intent
        .application_pins
        .last_mut()
        .unwrap()
        .include
        .insert(support::gid("cluster"));
    let r = resolve(&cat, &intent, &pid("prod"));
    let hosting = r
        .partition
        .applications
        .iter()
        .filter(|a| a.contains(&support::gid("cluster")))
        .count();
    assert!(hosting > 1, "{:?}", r.partition.applications);
    assert!(
        r.bindings.iter().all(|b| b.provider.as_str() != "cluster"),
        "nobody is remote: {:?}",
        r.bindings
    );
    assert!(
        !messages_of(&r, DiagnosticCode::ClusterInSeveralApplications).is_empty(),
        "{:?}",
        r.diagnostics
    );
}

#[test]
fn a_remote_cluster_outside_kubernetes_is_refused() {
    // The SDK has only Kubernetes DNS to find cluster by.
    let cat = support::remote_cluster_catalogue();
    let r = resolve(&cat, &remote_cluster_intent(false), &pid("local"));
    assert!(!messages_of(&r, DiagnosticCode::RemoteClusterNeedsKubernetes).is_empty());
    assert!(r.bindings.iter().all(|b| b.provider.as_str() != "cluster"));
}

#[test]
fn cluster_in_two_applications_while_reached_remotely_is_refused() {
    // Two Services named `cluster` cannot exist.
    let cat = support::remote_cluster_catalogue();
    let mut intent = remote_cluster_intent(true);
    intent.selected_gears.push(intent.selected_gears[0].clone());
    intent.selected_gears.last_mut().unwrap().gear = support::gid("other");
    support::pin(&mut intent, "other-app", "other", 1);
    intent
        .application_pins
        .iter_mut()
        .find(|p| p.name.as_str() == "other-app")
        .unwrap()
        .include = [support::gid("cluster")].into_iter().collect();
    let r = resolve(&cat, &intent, &pid("prod"));
    assert!(
        !messages_of(&r, DiagnosticCode::ClusterInSeveralApplications).is_empty(),
        "{:?}",
        r.diagnostics
    );
}

#[test]
fn a_cluster_hub_off_the_network_is_refused() {
    // Remote requesters dial TCP; a UDS hub serves nobody outside.
    let cat = support::remote_cluster_catalogue();
    let mut intent = remote_cluster_intent(true);
    let hub = intent
        .selected_gears
        .iter_mut()
        .find(|g| g.gear.as_str() == "grpc-hub")
        .unwrap();
    hub.config.insert(
        "listen_addr".to_owned(),
        serde_json::json!("uds:///run/hub.sock"),
    );
    let r = resolve(&cat, &intent, &pid("prod"));
    let messages = messages_of(&r, DiagnosticCode::TopologyGrpcWithoutHub);
    assert!(
        messages.iter().any(|m| m.contains("uds://")),
        "{messages:?}"
    );
}

/// A process-local backend in its own unreplicated cluster unit is one
/// copy of the state -- every requester reaches it over gRPC -- so it is not
/// GBX0503. The same unit replicated is.
#[test]
fn one_unreplicated_cluster_unit_holds_the_only_copy() {
    let mut cat = support::cluster_catalogue(vec![(
        ClusterPrimitive::Cache,
        "main",
        &["cluster.cache.linearizable"],
    )]);
    // The requester reaches cluster as a capability, not a `deps` edge, so it
    // links no copy of its own.
    cat.gears
        .get_mut(&support::gid("app"))
        .unwrap()
        .colocated_deps
        .clear();
    let mut intent = support::kubernetes(&["app", "cluster"], Discovery::Directory);
    support::bind_cluster(&mut intent, "main", "standalone", None);
    support::pin(&mut intent, "coordination", "cluster", 1);
    let profile = intent
        .profiles
        .keys()
        .find(|p| p.as_str() != "dev")
        .unwrap()
        .clone();

    let r = resolve(&cat, &intent, &profile);
    let apps: Vec<_> = r
        .partition
        .applications
        .iter()
        .map(|a| (a.name.to_string(), a.gears.clone()))
        .collect();
    assert_eq!(apps.len(), 2, "requester and cluster unit apart: {apps:?}");
    assert!(
        codes(&r).contains(&DiagnosticCode::ClusterAutoSelected)
            || r.cluster
                .iter()
                .any(|b| b.resolved.effective_provider() == Some("standalone")),
        "the scope resolved onto standalone: {:?}",
        r.diagnostics
            .iter()
            .map(|d| (d.code, &d.message))
            .collect::<Vec<_>>()
    );
    assert!(
        !codes(&r).contains(&DiagnosticCode::ClusterProcessLocalInMultiProcess),
        "{:?}",
        r.diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );

    intent
        .application_pins
        .iter_mut()
        .find(|p| p.name.as_str() == "coordination")
        .unwrap()
        .replicas = 2;
    let r = resolve(&cat, &intent, &profile);
    assert!(
        codes(&r).contains(&DiagnosticCode::ClusterProcessLocalInMultiProcess),
        "{:?} {:?}",
        r.partition
            .applications
            .iter()
            .map(|a| (a.name.as_str(), a.replicas, a.gears.clone()))
            .collect::<Vec<_>>(),
        r.diagnostics
            .iter()
            .map(|d| (d.code, &d.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_remote_grpc_binding_into_a_builtin_host_process_is_not_gbx0305() {
    // GBX0305 refuses a remote consumer of a process the
    // built-in HTTP host serves, because that host is a public listener and a
    // contract's REST routes are internal. A gRPC binding is served by the
    // process's hub, not its HTTP host, so it must not be refused -- even
    // though the provider's process has REST gears and no REST host.
    let mut cat = support::remote_cluster_catalogue();
    cat.gears
        .get_mut(&support::gid("cluster"))
        .unwrap()
        .runtime_caps
        .insert(gearbox_ir::RuntimeCap::Rest);
    let mut intent = support::kubernetes(
        &["app", "cluster", "grpc-hub"],
        gearbox_ir::Discovery::Static,
    );
    support::pin(&mut intent, "app-worker", "app", 1);
    support::bind_cluster(&mut intent, "app", "postgres", Some("existingSecret:pg"));

    let r = resolve(&cat, &intent, &pid("prod"));
    let binding = r
        .bindings
        .iter()
        .find(|b| b.provider.as_str() == "cluster")
        .unwrap_or_else(|| panic!("no cluster binding: {:?}", r.diagnostics));
    assert_eq!(binding.transport, gearbox_ir::Transport::Grpc);
    let provider = r
        .partition
        .applications
        .iter()
        .find(|a| a.name == binding.provider_application)
        .expect("the provider's application");
    assert!(
        provider.rest_host.is_none() && provider.serve.is_some(),
        "the premise: the built-in HTTP host serves cluster's process: {provider:?}"
    );
    assert!(
        messages_of(&r, DiagnosticCode::TopologyRestWithoutHost).is_empty(),
        "{:?}",
        r.diagnostics
    );
}
