//! Turning a catalogue plus an intent into a resolved product.
//!
//! Pure: no I/O, no clock, no environment. Deterministic by construction --
//! every map is a `BTreeMap`, every set a `BTreeSet`, and every output vector is
//! sorted by its identity tuple before it leaves. That is not tidiness. The lock
//! is hashed, and a hash that changes when nothing did is a hash nobody trusts.
//!
//! Errors do **not** abort. A product that fails one check still resolves as far
//! as it can, because a partial graph with three diagnostics is more useful than
//! one diagnostic and nothing to look at. Refusing to *write* the lock is a
//! separate decision, made by the caller.

pub mod bindings;
pub mod closure;
pub mod cluster;
pub mod cluster_options;
pub mod cuts;
pub mod database;
pub mod partition;
pub mod product;
pub mod profile;
pub mod structural;
mod ui;

use std::collections::BTreeSet;

use gearbox_ir::{
    Catalogue, Diagnostic, DiagnosticCode, Diagnostics, GearId, Location, Preference,
    ProductIntent, ProfileId,
};

/// A profile the description does not declare.
///
/// Reported rather than defaulted: resolving the wrong topology silently is the
/// one outcome worse than refusing.
fn unknown_profile(intent: &ProductIntent, profile: &ProfileId, uri: &str) -> Diagnostic {
    let declared = intent
        .profiles
        .keys()
        .map(ProfileId::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    Diagnostic::error(
        DiagnosticCode::GdlUnknownProfile,
        format!("`{profile}` is not a profile this product declares"),
        format!("the description declares: {declared}"),
    )
    .at(Location::file(uri.to_owned()))
}

/// Everything one resolution produced.
pub struct Resolution {
    pub ui: Option<gearbox_ir::ui::ResolvedUi>,
    pub database: Option<gearbox_ir::PostgresDatabase>,
    pub sqlite_databases:
        std::collections::BTreeMap<gearbox_ir::GearId, gearbox_ir::SqliteDatabase>,
    /// Which profile this is for.
    pub profile: ProfileId,
    /// The closure of gears, with why each is present.
    pub closure: closure::Closure,
    /// Which edges a process boundary may run through, and which may not.
    pub cuts: cuts::Cuts,
    /// The processes. Deliberately not a partition: closures overlap.
    pub partition: partition::Partition,
    /// How each severable edge is actually established.
    pub bindings: Vec<gearbox_ir::ResolvedBinding>,
    /// Which backend answers each cluster primitive.
    pub cluster: Vec<gearbox_ir::ResolvedClusterBinding>,
    pub diagnostics: Diagnostics,
}

impl Resolution {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics.has_errors()
    }
}

/// Resolve `intent` against `catalogue` for one deployment profile.
///
/// # Panics
/// Never. An unknown profile is a diagnostic, not a panic.
#[must_use]
pub fn resolve(catalogue: &Catalogue, intent: &ProductIntent, profile: &ProfileId) -> Resolution {
    resolve_at(catalogue, intent, profile, None)
}

/// As [`resolve`], but told where the product description lives on disk.
///
/// `product_path` only affects the diagnostics' URI, and it matters: a
/// `ProductIntent` carries `gdl_path` relative to its own root, so building
/// `file://` out of it yields `file://product.gdl` -- a URI that renders as a
/// link and opens nothing.
#[must_use]
pub fn resolve_at(
    catalogue: &Catalogue,
    intent: &ProductIntent,
    profile: &ProfileId,
    product_path: Option<&std::path::Path>,
) -> Resolution {
    let mut diagnostics = Diagnostics::new();

    // Computed before step 1, and that ordering is load-bearing. Steps 1 and 2
    // each used to build their own URI with `format!("file://{}", gdl_path)`,
    // which is the exact mistake the doc comment above this function warns about
    // -- a relative path in a URI reads its first segment as the *host*, so
    // `products/demo/product.gdl` becomes host `products` and a path that opens
    // nothing. Every diagnostic they raised was therefore unopenable, and
    // invisible to the language server besides: `lsp::publishable` matches
    // `location.uri` against the document's own URI, so a wrong URI discards the
    // diagnostic however good its range is.
    let uri = match product_path {
        Some(path) => gearbox_ir::file_uri(path),
        None => gearbox_ir::file_uri(std::path::Path::new(intent.gdl_path.as_str())),
    };

    // Step 1 -- narrow to the profile. Done first because everything after it
    // reads the narrowed view, and because a duplicate that only appears once
    // narrowed is a contradiction the description could not have shown.
    let scoped = profile::scope(intent, profile, &uri, &mut diagnostics);

    // Step 2 -- the co-location closure.
    let closure = closure::expand(catalogue, intent, profile, &uri, &mut diagnostics);
    // What the product links below `stable` -- known now, for every member.
    closure::maturity_diagnostics(
        catalogue,
        closure.members.iter().map(|(g, r)| (g, r.as_slice())),
        intent,
        &uri,
        &mut diagnostics,
    );

    let database = intent.profiles.get(profile).and_then(|declaration| {
        database::resolve(catalogue, &closure, declaration, &uri, &mut diagnostics)
    });

    // Step 3 -- which edges could carry a boundary.
    let cuts = cuts::classify(catalogue, &closure, &uri, &mut diagnostics);

    // Not a resolver step: a join between the product's config values and the
    // types the catalogue projected for them, which needs no resolution at all.
    // It runs here as well as in `validate` because the Add Gear panel resolves
    // rather than validates, and a check only `validate` performed would never
    // reach the person setting the value. `Diagnostics::finish` dedups.
    crate::config_check::check(catalogue, intent, &uri, &mut diagnostics);
    // The same shape, for features: a gear declares which deployment kinds a
    // feature belongs to, and only a resolution knows which one is being built.
    crate::feature_check::check(catalogue, intent, &[profile], &uri, &mut diagnostics);
    // **On this path too, and that is the point of it.** `validate` alone would
    // leave the one caller that matters uncovered: `generate` and the Studio
    // both come through here, and a binding to a provider the build will not
    // contain resolves perfectly -- the lock records it and the process fails at
    // startup, or, for leader election, does not fail and elects one leader per
    // replica.
    crate::provider_feature_check::check(catalogue, intent, &uri, &mut diagnostics);
    // And the plugin joins, for the same reason: an unfilled extension point
    // (GBX0511) and a shared lowest priority (GBX0517) were `validate`-only, so
    // the Studio said "errors 0" and `generate` wrote a tree for a product whose
    // host finds no implementation at its first request.
    drop(crate::plugin_select::check_profiles(
        catalogue,
        intent,
        &[profile],
        &uri,
        &mut diagnostics,
    ));

    // Step 4 -- processes. An unknown profile is reported rather than assumed,
    // because guessing `embedded` would silently resolve the wrong topology.
    // Includes this profile's plugins: a plugin is a gear the description named,
    // and a process seeded without it leaves it in the product and in no binary.
    for preference in &intent.preferences {
        match preference {
            Preference::ExistingInfrastructure | Preference::Isolate { .. } => {}
            Preference::FewerApplications => {
                diagnostics.push(
                    Diagnostic::new(
                        DiagnosticCode::PreferenceNotHonoured,
                        "`prefer.fewer_applications` is recorded but the resolver does not honour it",
                    )
                    .with_help(
                        "remove it, or wait until application packing is implemented; it does not \
                         change the topology today",
                    )
                    .at(Location::file(&uri)),
                );
            }
        }
    }

    let selected = closure::seeds(intent, profile);
    let isolates: BTreeSet<GearId> = intent
        .preferences
        .iter()
        .filter_map(|preference| match preference {
            Preference::Isolate { gear } => Some(gear.clone()),
            Preference::ExistingInfrastructure | Preference::FewerApplications => None,
        })
        .collect();
    let input = partition::Inputs {
        catalogue,
        closure: &closure,
        cuts: &cuts,
        scoped: &scoped,
        selected: &selected,
        isolates: &isolates,
    };
    let partition = if let Some(declaration) = intent.profiles.get(profile) {
        let partition = partition::partition(
            &input,
            declaration,
            &intent.id,
            &intent.version,
            &uri,
            &mut diagnostics,
        );
        // Step 5 -- what the runtime will refuse, said before a binary exists.
        structural::check(catalogue, &partition, declaration, &uri, &mut diagnostics);
        database::check_sqlite_placement(&partition, declaration, &uri, &mut diagnostics);
        partition
    } else {
        diagnostics.push(unknown_profile(intent, profile, &uri));
        partition::Partition::default()
    };

    // Step 6 -- bindings, derived from placement rather than declared.
    let bindings = intent.profiles.get(profile).map_or_else(Vec::new, |d| {
        let mut derived = bindings::derive(
            catalogue,
            &cuts,
            &partition,
            &scoped,
            d,
            &uri,
            &mut diagnostics,
        );
        bindings::pin_static_endpoints(&mut derived, &partition, d);
        bindings::report_env_limits(&derived, &uri, &mut diagnostics);
        bindings::report_wiring_key_skew(catalogue, &derived, &uri, &mut diagnostics);
        derived
    });

    // Steps 7 and 8 -- cluster primitives, and the replication they imply.
    let cluster = cluster::resolve(
        catalogue,
        &closure,
        &partition,
        &scoped,
        intent,
        &uri,
        &mut diagnostics,
    );
    cluster::report_stateful_replicas(catalogue, &partition, &cluster, &uri, &mut diagnostics);

    let ui = ui::resolve(catalogue, intent, &closure, &uri, &mut diagnostics);
    diagnostics.finish();
    Resolution {
        ui,
        database,
        sqlite_databases: intent
            .profiles
            .get(profile)
            .map(|decl| decl.sqlite_databases().clone())
            .unwrap_or_default(),
        profile: profile.clone(),
        closure,
        cuts,
        partition,
        bindings,
        cluster,
        diagnostics,
    }
}
