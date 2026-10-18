//! Structural diffing between two resolved products.
//!
//! A text diff of two `product.lock` files is dominated by noise: changing
//! one intent value can shift every line below it if a collection gains or
//! loses an entry. This compares the parsed structures instead, so the diff
//! is confined to what actually changed
//! (`cpt-gearbox-nfr-lock-diff-minimal`).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use gearbox_ir::{
    ApplicationId, ClusterPrimitive, ContractId, CutCandidate, GearId, NodeId, ProvenanceEdge,
    ProvenanceKind, ResolvedApplication, ResolvedBinding, ResolvedClusterBinding, ResolvedProduct,
    SourceId,
};
use serde::Serialize;

use crate::canonical::canonicalize_order;

/// The `(consumer, contract)` pair that identifies one binding.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct BindingKey {
    pub consumer: GearId,
    pub contract: ContractId,
}

/// The `(scope, primitive)` pair that identifies one cluster binding.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ClusterKey {
    pub scope: String,
    pub primitive: ClusterPrimitive,
}

/// The `(consumer, provider, contract)` triple that identifies one blocked cut.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CutKey {
    pub consumer: GearId,
    pub provider: GearId,
    pub contract: Option<ContractId>,
}

/// The `(from, kind, to)` triple that identifies one provenance edge.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ProvenanceKey {
    pub from: NodeId,
    pub kind: ProvenanceKind,
    pub to: NodeId,
}

/// One scalar field of the lock that differs -- `(before, after)`.
///
/// A list rather than one `Option` per field: the header alone has six, and six
/// nearly-identical `Option<(String, String)>` members would be six chances to
/// forget one in [`LockDiff::is_empty`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct FieldChange {
    /// Dotted path, e.g. `product.lock_hash`.
    pub field: String,
    pub before: String,
    pub after: String,
}

/// What changed between two resolved products.
///
/// Every list is sorted, so the diff itself is deterministic. A key appears
/// in at most one of the added/removed/changed lists for its category.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LockDiff {
    /// Set when the profile itself differs -- `(before, after)`.
    pub profile_changed: Option<(String, String)>,

    /// Every other scalar that differs: the rest of the header, and the
    /// Kubernetes block. Sorted by field name.
    ///
    /// These used to be compared by nothing at all, so `is_empty` answered
    /// "nothing changed" for a lock with a different `lock_hash`, a different
    /// namespace, or a different image registry.
    pub fields_changed: Vec<FieldChange>,

    pub sources_added: Vec<SourceId>,
    pub sources_removed: Vec<SourceId>,
    pub sources_changed: Vec<SourceId>,

    pub gears_added: Vec<GearId>,
    pub gears_removed: Vec<GearId>,
    pub gears_changed: Vec<GearId>,

    pub applications_added: Vec<ApplicationId>,
    pub applications_removed: Vec<ApplicationId>,
    pub applications_changed: Vec<ApplicationId>,

    pub bindings_added: Vec<BindingKey>,
    pub bindings_removed: Vec<BindingKey>,
    pub bindings_changed: Vec<BindingKey>,

    pub cluster_added: Vec<ClusterKey>,
    pub cluster_removed: Vec<ClusterKey>,
    pub cluster_changed: Vec<ClusterKey>,

    pub cuts_added: Vec<CutKey>,
    pub cuts_removed: Vec<CutKey>,
    pub cuts_changed: Vec<CutKey>,

    pub provenance_added: Vec<ProvenanceKey>,
    pub provenance_removed: Vec<ProvenanceKey>,
    pub provenance_changed: Vec<ProvenanceKey>,

    /// Set when the two locks carry different diagnostics -- `(before, after)`
    /// counts.
    ///
    /// Counts rather than the diagnostics themselves: the widget's job is to
    /// say that the advice changed, and reproducing it here would duplicate a
    /// list the caller already has.
    pub diagnostics_changed: Option<(usize, usize)>,
}

impl LockDiff {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        let Self {
            profile_changed,
            fields_changed,
            sources_added,
            sources_removed,
            sources_changed,
            gears_added,
            gears_removed,
            gears_changed,
            applications_added,
            applications_removed,
            applications_changed,
            bindings_added,
            bindings_removed,
            bindings_changed,
            cluster_added,
            cluster_removed,
            cluster_changed,
            cuts_added,
            cuts_removed,
            cuts_changed,
            provenance_added,
            provenance_removed,
            provenance_changed,
            diagnostics_changed,
        } = self;
        profile_changed.is_none()
            && fields_changed.is_empty()
            && diagnostics_changed.is_none()
            && sources_added.is_empty()
            && sources_removed.is_empty()
            && sources_changed.is_empty()
            && cuts_added.is_empty()
            && cuts_removed.is_empty()
            && cuts_changed.is_empty()
            && provenance_added.is_empty()
            && provenance_removed.is_empty()
            && provenance_changed.is_empty()
            && gears_added.is_empty()
            && gears_removed.is_empty()
            && gears_changed.is_empty()
            && applications_added.is_empty()
            && applications_removed.is_empty()
            && applications_changed.is_empty()
            && bindings_added.is_empty()
            && bindings_removed.is_empty()
            && bindings_changed.is_empty()
            && cluster_added.is_empty()
            && cluster_removed.is_empty()
            && cluster_changed.is_empty()
    }

    /// Render as ordered, human-readable lines: `+` added, `-` removed,
    /// `~` changed. What a CLI or the Studio's Lock widget shows directly.
    #[must_use]
    pub fn summary(&self) -> Vec<String> {
        let mut lines = Vec::new();

        if let Some((before, after)) = &self.profile_changed {
            lines.push(format!(
                "~ profile: {} -> {}",
                one_line(before),
                one_line(after)
            ));
        }
        for change in &self.fields_changed {
            lines.push(format!(
                "~ {}: {} -> {}",
                one_line(&change.field),
                one_line(&change.before),
                one_line(&change.after)
            ));
        }
        if let Some((before, after)) = self.diagnostics_changed {
            if before == after {
                lines.push(format!("~ diagnostics changed ({before})"));
            } else {
                lines.push(format!("~ diagnostics: {before} -> {after}"));
            }
        }

        marked(
            &mut lines,
            "source",
            [
                &self.sources_added,
                &self.sources_removed,
                &self.sources_changed,
            ],
            ToString::to_string,
        );
        marked(
            &mut lines,
            "gear",
            [&self.gears_added, &self.gears_removed, &self.gears_changed],
            ToString::to_string,
        );
        marked(
            &mut lines,
            "application",
            [
                &self.applications_added,
                &self.applications_removed,
                &self.applications_changed,
            ],
            ToString::to_string,
        );
        marked(
            &mut lines,
            "binding",
            [
                &self.bindings_added,
                &self.bindings_removed,
                &self.bindings_changed,
            ],
            |key| format!("{} -> {}", key.consumer, key.contract),
        );
        marked(
            &mut lines,
            "cluster",
            [
                &self.cluster_added,
                &self.cluster_removed,
                &self.cluster_changed,
            ],
            |key| format!("{}.{}", one_line(&key.scope), key.primitive),
        );
        marked(
            &mut lines,
            "cuttable",
            [&self.cuts_added, &self.cuts_removed, &self.cuts_changed],
            |key| {
                let contract = key
                    .contract
                    .as_ref()
                    .map_or_else(String::new, |c| format!(" : {c}"));
                format!("{} -> {}{contract}", key.consumer, key.provider)
            },
        );
        marked(
            &mut lines,
            "provenance",
            [
                &self.provenance_added,
                &self.provenance_removed,
                &self.provenance_changed,
            ],
            |key| {
                format!(
                    "{} {} {}",
                    key.to.as_str(),
                    key.kind.phrase(),
                    key.from.as_str()
                )
            },
        );

        lines
    }
}

/// Every scalar in the lock that is not a collection and not the profile.
///
/// Spelled out one pair at a time rather than diffed from serialized JSON: a
/// structural diff that reached for `serde_json::Value` would report a changed
/// *collection* here as well, and each category already has its own list.
fn scalar_fields(before: &ResolvedProduct, after: &ResolvedProduct) -> Vec<FieldChange> {
    let mut out = Vec::new();
    let mut compare = |field: &str, a: String, b: String| {
        if a != b {
            out.push(FieldChange {
                field: field.to_owned(),
                before: a,
                after: b,
            });
        }
    };

    // Destructure so a new lock field is a compile error here, not a silent
    // "unchanged". Collections and diagnostics have their own lists; profile
    // is `profile_changed`.
    let gearbox_ir::ResolvedProduct {
        ui: before_ui,
        sqlite_databases: before_sqlite,
        database: before_db,
        schema_version: before_schema,
        product: before_header,
        kubernetes: before_k8s,
        self_hosted: before_hw,
        sources: _,
        gears: _,
        applications: _,
        bindings: _,
        cluster: _,
        cuttable_if_declared: _,
        provenance: _,
        diagnostics: _,
    } = before;
    let gearbox_ir::ResolvedProduct {
        ui: after_ui,
        sqlite_databases: after_sqlite,
        database: after_db,
        schema_version: after_schema,
        product: after_header,
        kubernetes: after_k8s,
        self_hosted: after_hw,
        sources: _,
        gears: _,
        applications: _,
        bindings: _,
        cluster: _,
        cuttable_if_declared: _,
        provenance: _,
        diagnostics: _,
    } = after;

    let mut before_database = database_fields(before_db.as_ref());
    let mut after_database = database_fields(after_db.as_ref());
    for (fields, sqlite) in [
        (&mut before_database, before_sqlite),
        (&mut after_database, after_sqlite),
    ] {
        fields.extend(
            sqlite
                .iter()
                .map(|(id, db)| (format!("sqlite_databases.{id}.file"), db.file.clone())),
        );
    }
    let mut database_keys = std::collections::BTreeSet::new();
    database_keys.extend(before_database.keys());
    database_keys.extend(after_database.keys());
    for key in database_keys {
        compare(
            key,
            before_database.get(key).cloned().unwrap_or_default(),
            after_database.get(key).cloned().unwrap_or_default(),
        );
    }

    compare("ui", format!("{before_ui:?}"), format!("{after_ui:?}"));
    compare(
        "schema_version",
        before_schema.to_string(),
        after_schema.to_string(),
    );

    let header_scalars = |header: &gearbox_ir::ResolvedProductHeader| {
        let gearbox_ir::ResolvedProductHeader {
            id,
            version,
            profile: _,
            profile_kind,
            layout,
            gearbox_version,
            lock_hash,
        } = header;
        (
            id.clone(),
            version.clone(),
            profile_kind.clone(),
            layout.clone(),
            gearbox_version.clone(),
            lock_hash.clone(),
        )
    };
    let (bid, bver, bkind, blayout, beng, bhash) = header_scalars(before_header);
    let (aid, aver, akind, alayout, aeng, ahash) = header_scalars(after_header);
    compare("product.id", bid, aid);
    compare("product.version", bver, aver);
    compare("product.profile_kind", bkind, akind);
    // Reported like any other header scalar. A layout change moves every
    // application crate, so a diff that stayed silent about it would show a
    // tree of creations with no stated cause.
    compare("product.layout", blayout, alayout);
    compare("product.gearbox_version", beng, aeng);
    compare("product.lock_hash", bhash, ahash);

    let k8s_scalars = |settings: Option<&gearbox_ir::KubernetesSettings>| {
        let Some(gearbox_ir::KubernetesSettings {
            namespace,
            image_registry,
            discovery,
        }) = settings
        else {
            return (String::new(), String::new(), String::new());
        };
        (
            namespace.clone().unwrap_or_default(),
            image_registry.clone().unwrap_or_default(),
            discovery.as_str().to_owned(),
        )
    };
    let (bns, breg, bdisc) = k8s_scalars(before_k8s.as_ref());
    let (ans, areg, adisc) = k8s_scalars(after_k8s.as_ref());
    compare("kubernetes.namespace", bns, ans);
    compare("kubernetes.image_registry", breg, areg);
    compare("kubernetes.discovery", bdisc, adisc);

    let hw_scalars = |settings: Option<&gearbox_ir::SelfHostedSettings>| {
        let Some(gearbox_ir::SelfHostedSettings {
            target_dir,
            cargo_profile,
            discovery,
        }) = settings
        else {
            return (String::new(), String::new(), String::new());
        };
        (
            target_dir.clone().unwrap_or_default(),
            cargo_profile.clone().unwrap_or_default(),
            discovery.as_str().to_owned(),
        )
    };
    let (btd, bcp, bhd) = hw_scalars(before_hw.as_ref());
    let (atd, acp, ahd) = hw_scalars(after_hw.as_ref());
    compare("self_hosted.target_dir", btd, atd);
    compare("self_hosted.cargo_profile", bcp, acp);
    compare("self_hosted.discovery", bhd, ahd);

    out.sort();
    out
}

fn database_fields(
    database: Option<&gearbox_ir::PostgresDatabase>,
) -> std::collections::BTreeMap<String, String> {
    let Some(gearbox_ir::PostgresDatabase {
        host,
        port,
        user,
        password,
        params,
        database_names,
        existing_secret,
    }) = database
    else {
        return std::collections::BTreeMap::new();
    };
    let mut fields = std::collections::BTreeMap::from([
        ("database.host".to_owned(), host.clone()),
        ("database.port".to_owned(), port.to_string()),
        ("database.user".to_owned(), user.clone()),
        ("database.password".to_owned(), password.clone()),
        (
            "database.existing_secret".to_owned(),
            existing_secret.clone().unwrap_or_default(),
        ),
    ]);
    fields.extend(
        params
            .iter()
            .map(|(k, v)| (format!("database.params.{k}"), v.clone())),
    );
    fields.extend(
        database_names
            .iter()
            .map(|(k, v)| (format!("database.database_names.{k}"), v.clone())),
    );
    fields
}

/// Append `+`/`-`/`~` lines for one category, in that order.
///
/// One helper rather than three loops per category: with seven categories the
/// loops were twenty-one near-identical blocks, and the only thing distinguishing
/// them -- the label and how a key reads -- is exactly what is passed in.
fn marked<K>(
    lines: &mut Vec<String>,
    label: &str,
    keys: [&Vec<K>; 3],
    render: impl Fn(&K) -> String,
) {
    for (mark, list) in ["+", "-", "~"].into_iter().zip(keys) {
        for key in list {
            lines.push(format!("{mark} {label} {}", render(key)));
        }
    }
}

/// Render a lock scalar as one line of output.
///
/// The ids in a lock are newtypes that refuse control characters. The values
/// these lines interpolate -- `product.id`, `kubernetes.namespace`, a cluster
/// scope -- are plain `String`s straight out of a parsed file, so one of them
/// can carry a newline or an escape sequence and forge or erase a line in the
/// summary someone reads to decide whether a lock change is acceptable.
fn one_line(value: &str) -> String {
    if !value.chars().any(char::is_control) {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        // `escape_debug` is the same spelling Rust prints: `\n` for a
        // newline, `\u{1b}` for an escape byte. Applied to control characters
        // only, so the rest of the value reads as it was written.
        if c.is_control() {
            out.extend(c.escape_debug());
        } else {
            out.push(c);
        }
    }
    out
}

/// The added, removed, and changed keys between two ordered maps, each list
/// sorted (an artifact of `K: Ord` and `BTreeMap`'s iteration order, not an
/// extra sort pass).
///
/// One merge walk over two already-sorted iterators: indexing the maps again
/// for the shared keys, or collecting their keys into sets to take a
/// difference, would re-sort what `BTreeMap` already handed over in order.
fn diff_keyed<K: Ord + Clone, V: PartialEq>(
    before: &BTreeMap<K, V>,
    after: &BTreeMap<K, V>,
) -> (Vec<K>, Vec<K>, Vec<K>) {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    let mut before = before.iter().peekable();
    let mut after = after.iter().peekable();
    loop {
        match (before.peek(), after.peek()) {
            (Some((before_key, before_value)), Some((after_key, after_value))) => {
                match before_key.cmp(after_key) {
                    Ordering::Less => {
                        removed.push((*before_key).clone());
                        before.next();
                    }
                    Ordering::Greater => {
                        added.push((*after_key).clone());
                        after.next();
                    }
                    Ordering::Equal => {
                        if before_value != after_value {
                            changed.push((*before_key).clone());
                        }
                        before.next();
                        after.next();
                    }
                }
            }
            (Some((before_key, _)), None) => {
                removed.push((*before_key).clone());
                before.next();
            }
            (None, Some((after_key, _))) => {
                added.push((*after_key).clone());
                after.next();
            }
            (None, None) => break,
        }
    }

    (added, removed, changed)
}

/// Index a product's entries by key, keeping every entry that shares one.
///
/// **Not one value per key, and that is the fix for a dropped change.** None
/// of these keys is unique over its collection: `canonicalize_order` sorts
/// cut candidates by `(consumer, provider, contract)` and provenance edges by
/// `(from, kind, to)`, and drops only entries equal in *all* their fields, so
/// two entries sharing a key and differing elsewhere both reach the lock.
/// Keeping one value per key meant a change to the shadowed entry appeared in
/// no list and [`LockDiff::is_empty`] then said nothing had changed.
fn grouped<'a, K: Ord, V: 'a>(
    entries: impl Iterator<Item = (K, &'a V)>,
) -> BTreeMap<K, Vec<&'a V>> {
    let mut out: BTreeMap<K, Vec<&'a V>> = BTreeMap::new();
    for (key, value) in entries {
        out.entry(key).or_default().push(value);
    }
    out
}

// The five index helpers below borrow their entries rather than cloning them:
// the maps exist only for the `!=` inside `diff_keyed`, and cloning an
// application copies its gears, listens, spawns and feature sets -- or, for
// provenance, every edge of the largest collection in a real lock -- for one
// comparison.

fn by_application_name(
    product: &ResolvedProduct,
) -> BTreeMap<ApplicationId, Vec<&ResolvedApplication>> {
    grouped(product.applications.iter().map(|p| (p.name.clone(), p)))
}

fn by_binding_key(product: &ResolvedProduct) -> BTreeMap<BindingKey, Vec<&ResolvedBinding>> {
    grouped(product.bindings.iter().map(|b| {
        (
            BindingKey {
                consumer: b.consumer.clone(),
                contract: b.contract.clone(),
            },
            b,
        )
    }))
}

fn by_cluster_key(product: &ResolvedProduct) -> BTreeMap<ClusterKey, Vec<&ResolvedClusterBinding>> {
    grouped(product.cluster.iter().map(|c| {
        (
            ClusterKey {
                scope: c.scope.clone(),
                primitive: c.primitive,
            },
            c,
        )
    }))
}

fn by_cut_key(product: &ResolvedProduct) -> BTreeMap<CutKey, Vec<&CutCandidate>> {
    grouped(product.cuttable_if_declared.iter().map(|c| {
        (
            CutKey {
                consumer: c.consumer.clone(),
                provider: c.provider.clone(),
                contract: c.contract.clone(),
            },
            c,
        )
    }))
}

fn by_provenance_key(product: &ResolvedProduct) -> BTreeMap<ProvenanceKey, Vec<&ProvenanceEdge>> {
    grouped(product.provenance.iter().map(|e| {
        (
            ProvenanceKey {
                from: e.from.clone(),
                kind: e.kind,
                to: e.to.clone(),
            },
            e,
        )
    }))
}

/// Compare two resolved products structurally.
///
/// Both sides are canonicalized first, so the answer is about content and
/// nothing else. Ordering inside a nested collection -- `listens`, `spawns`,
/// `selected_by`, a cluster entry's `requesters` -- is not content, and the
/// ordinary call has one side straight from the resolver and the other from
/// [`read`](crate::read), which returns a canonicalized product: without this
/// pass every such pair came out as changed.
///
/// Entries are grouped by key rather than indexed by it, so two entries that
/// share a key are compared instead of one shadowing the other.
#[must_use]
pub fn diff(before: &ResolvedProduct, after: &ResolvedProduct) -> LockDiff {
    let mut before_canonical = before.clone();
    canonicalize_order(&mut before_canonical);
    let mut after_canonical = after.clone();
    canonicalize_order(&mut after_canonical);
    let (before, after) = (&before_canonical, &after_canonical);

    let profile_changed = (before.product.profile != after.product.profile).then(|| {
        (
            before.product.profile.to_string(),
            after.product.profile.to_string(),
        )
    });

    let fields_changed = scalar_fields(before, after);

    let (sources_added, sources_removed, sources_changed) =
        diff_keyed(&before.sources, &after.sources);

    let (gears_added, gears_removed, gears_changed) = diff_keyed(&before.gears, &after.gears);

    let (applications_added, applications_removed, applications_changed) =
        diff_keyed(&by_application_name(before), &by_application_name(after));

    let (bindings_added, bindings_removed, bindings_changed) =
        diff_keyed(&by_binding_key(before), &by_binding_key(after));

    let (cluster_added, cluster_removed, cluster_changed) =
        diff_keyed(&by_cluster_key(before), &by_cluster_key(after));

    let (cuts_added, cuts_removed, cuts_changed) =
        diff_keyed(&by_cut_key(before), &by_cut_key(after));

    let (provenance_added, provenance_removed, provenance_changed) =
        diff_keyed(&by_provenance_key(before), &by_provenance_key(after));

    // Already sorted and deduplicated by the canonicalization above, so the
    // comparison is of content rather than of the order two runs happened to
    // report the same advice in.
    let diagnostics_changed = (before.diagnostics != after.diagnostics)
        .then(|| (before.diagnostics.len(), after.diagnostics.len()));

    LockDiff {
        profile_changed,
        fields_changed,
        sources_added,
        sources_removed,
        sources_changed,
        gears_added,
        gears_removed,
        gears_changed,
        applications_added,
        applications_removed,
        applications_changed,
        bindings_added,
        bindings_removed,
        bindings_changed,
        cluster_added,
        cluster_removed,
        cluster_changed,
        cuts_added,
        cuts_removed,
        cuts_changed,
        provenance_added,
        provenance_removed,
        provenance_changed,
        diagnostics_changed,
    }
}
