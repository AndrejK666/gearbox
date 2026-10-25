//! Structural diffing: minimal, deterministic, and sorted -- not a
//! reformatted-everything text diff (`cpt-gearbox-nfr-lock-diff-minimal`).

#![allow(
    clippy::unwrap_used,
    reason = "clippy.toml's allow-unwrap-in-tests covers `#[test]` functions but not the \
              helpers in this file"
)]

mod support;

use gearbox_ir::{ApplicationId, ClusterPrimitive, ContractId, GearId};

#[test]
fn identical_products_diff_to_nothing() {
    let a = support::fixture();
    let b = support::fixture();
    let d = gearbox_lock::diff(&a, &b);
    assert!(d.is_empty(), "{d:?}");
    assert!(d.summary().is_empty());
}

#[test]
fn diff_is_order_independent() {
    // The whole reason to diff structures instead of text: shuffling one
    // side must not manufacture spurious changes.
    let a = support::fixture();
    let mut b = support::fixture();
    support::shuffle_orderings(&mut b, 7);

    let d = gearbox_lock::diff(&a, &b);
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn detects_an_added_gear() {
    let before = support::fixture();
    let mut after = support::fixture();

    // Adding service-discovery as a new, unrelated gear and process (not
    // wired into any binding) isolates the "added" case from "changed".
    let id = GearId::new("service-discovery").unwrap();
    let mut gear = after
        .gears
        .get(&GearId::new("api-contracts").unwrap())
        .unwrap()
        .clone();
    gear.id = id.clone();
    after.gears.insert(id, gear);

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(
        d.gears_added,
        vec![GearId::new("service-discovery").unwrap()]
    );
    assert!(d.gears_removed.is_empty());
    assert!(d.gears_changed.is_empty());
    assert!(!d.is_empty());
}

#[test]
fn detects_a_removed_gear() {
    let before = support::fixture();
    let mut after = support::fixture();
    after.gears.remove(&GearId::new("types-registry").unwrap());

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(
        d.gears_removed,
        vec![GearId::new("types-registry").unwrap()]
    );
    assert!(d.gears_added.is_empty());
}

#[test]
fn detects_a_changed_gear() {
    let before = support::fixture();
    let mut after = support::fixture();
    after
        .gears
        .get_mut(&GearId::new("cluster").unwrap())
        .unwrap()
        .runtime_caps
        .insert(gearbox_ir::RuntimeCap::Db);

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(d.gears_changed, vec![GearId::new("cluster").unwrap()]);
    assert!(d.gears_added.is_empty());
    assert!(d.gears_removed.is_empty());
}

#[test]
fn detects_process_and_binding_changes() {
    let before = support::fixture();
    let mut after = support::fixture();

    after
        .applications
        .iter_mut()
        .find(|p| p.name == ApplicationId::new("payments-audit").unwrap())
        .unwrap()
        .replicas = 2;
    after.bindings[0].critical = true;

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(
        d.applications_changed,
        vec![ApplicationId::new("payments-audit").unwrap()]
    );
    assert!(d.applications_added.is_empty());
    assert!(d.applications_removed.is_empty());

    assert_eq!(d.bindings_changed.len(), 1);
    assert_eq!(
        d.bindings_changed[0].consumer,
        GearId::new("payments-audit").unwrap()
    );
    assert_eq!(
        d.bindings_changed[0].contract,
        ContractId::new("api-contracts/PaymentApi@v1").unwrap()
    );
}

#[test]
fn detects_cluster_changes_keyed_by_scope_and_primitive() {
    let before = support::fixture();
    let mut after = support::fixture();
    after
        .cluster
        .iter_mut()
        .find(|c| c.primitive == ClusterPrimitive::Cache)
        .unwrap()
        .secret_ref = Some("existingSecret:different".to_owned());

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(d.cluster_changed.len(), 1);
    assert_eq!(d.cluster_changed[0].scope, "default");
    assert_eq!(d.cluster_changed[0].primitive, ClusterPrimitive::Cache);
    assert!(d.cluster_added.is_empty());
    assert!(d.cluster_removed.is_empty());
}

#[test]
fn detects_a_profile_change() {
    let before = support::fixture();
    let mut after = support::fixture();
    after.product.profile = gearbox_ir::ProfileId::new("prod").unwrap();

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(
        d.profile_changed,
        Some(("local".to_owned(), "prod".to_owned()))
    );
}

#[test]
fn summary_lines_are_sorted_and_readable() {
    let before = support::fixture();
    let mut after = support::fixture();
    // Two gears, removed in the order that is *not* their name order, so the
    // sortedness the diff documents is what puts the lines in order rather
    // than the order they were touched in.
    after.gears.remove(&GearId::new("types-registry").unwrap());
    after.gears.remove(&GearId::new("api-contracts").unwrap());
    after.bindings[0].critical = true;

    let d = gearbox_lock::diff(&before, &after);
    let summary = d.summary();

    let removed: Vec<&String> = summary
        .iter()
        .filter(|l| l.starts_with("- gear "))
        .collect();
    assert_eq!(
        removed,
        ["- gear api-contracts", "- gear types-registry"],
        "{summary:?}"
    );
    assert!(
        summary
            .iter()
            .any(|l| l.starts_with("~ binding payments-audit -> ")),
        "{summary:?}"
    );
}

#[test]
fn summary_renders_a_cut_candidate_with_and_without_its_contract() {
    let before = support::fixture();
    let mut after = support::fixture();
    after.cuttable_if_declared.clear();

    let summary = gearbox_lock::diff(&before, &after).summary();
    let cuttable: Vec<&String> = summary
        .iter()
        .filter(|l| l.starts_with("- cuttable "))
        .collect();
    assert_eq!(
        cuttable,
        [
            "- cuttable api-gateway -> payments-audit",
            "- cuttable api-gateway -> types-registry : types-registry/TypeRegistry@v1",
        ],
        "{summary:?}"
    );
}

#[test]
fn control_characters_in_a_lock_scalar_cannot_forge_a_summary_line() {
    // `product.id` is a plain String out of a parsed file, unlike the id
    // newtypes that refuse control characters. A summary someone reads to
    // decide whether a lock change is acceptable must not be forgeable by the
    // lock being described.
    let before = support::fixture();
    let mut after = support::fixture();
    after.product.id = "payments-demo\n+ gear a-gear-nobody-added".to_owned();

    let summary = gearbox_lock::diff(&before, &after).summary();
    assert_eq!(summary.len(), 1, "{summary:?}");
    assert!(summary[0].contains("\\n"), "{summary:?}");
    assert!(!summary[0].contains('\n'), "{summary:?}");
}

#[test]
fn detects_a_changed_source_pin() {
    // The pin is what makes a lock reproducible. A diff that reported "no
    // change" across a re-pinned source would hide the one thing an operator
    // reviews a lock for.
    let before = support::fixture();
    let mut after = support::fixture();
    let id = gearbox_ir::SourceId::new("gears-rust").unwrap();
    after.sources.get_mut(&id).unwrap().digest = "git:0000000000000000".to_owned();

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(d.sources_changed, vec![id]);
    assert!(d.sources_added.is_empty());
    assert!(!d.is_empty());
    assert!(d.summary().iter().any(|l| l == "~ source gears-rust"));
}

#[test]
fn detects_a_changed_lock_hash() {
    let before = support::fixture();
    let mut after = support::fixture();
    after.product.lock_hash = "blake3:deadbeef".to_owned();

    let d = gearbox_lock::diff(&before, &after);
    assert!(!d.is_empty(), "{d:?}");
    assert_eq!(
        d.fields_changed
            .iter()
            .map(|c| c.field.as_str())
            .collect::<Vec<_>>(),
        vec!["product.lock_hash"]
    );
}

#[test]
fn detects_changed_kubernetes_settings() {
    let before = support::fixture();
    let mut after = support::fixture();
    after.kubernetes = Some(gearbox_ir::KubernetesSettings {
        namespace: Some("payments".to_owned()),
        image_registry: None,
        discovery: gearbox_ir::Discovery::Static,
    });

    let d = gearbox_lock::diff(&before, &after);
    assert!(!d.is_empty(), "{d:?}");
    assert!(
        d.fields_changed
            .iter()
            .any(|c| c.field == "kubernetes.namespace" && c.after == "payments"),
        "{:?}",
        d.fields_changed
    );
}

#[test]
fn detects_a_removed_cut_candidate() {
    let before = support::fixture();
    let mut after = support::fixture();
    let dropped = after.cuttable_if_declared.remove(0);

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(
        d.cuts_removed,
        vec![gearbox_lock::CutKey {
            consumer: dropped.consumer,
            provider: dropped.provider,
            contract: dropped.contract,
        }]
    );
    assert!(!d.is_empty());
}

#[test]
fn detects_a_changed_provenance_reason() {
    // The edge is the same edge; only the recorded "why" moved. That is still a
    // lock mutation, and a widget claiming otherwise would be wrong.
    let before = support::fixture();
    let mut after = support::fixture();
    after.provenance[0].because = "a different reason entirely".to_owned();

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(d.provenance_changed.len(), 1, "{d:?}");
    assert!(d.provenance_added.is_empty());
    assert!(!d.is_empty());
}

#[test]
fn detects_changed_diagnostics() {
    let before = support::fixture();
    let mut after = support::fixture();
    after.diagnostics = before.diagnostics.iter().take(1).cloned().collect();

    let d = gearbox_lock::diff(&before, &after);
    assert!(d.diagnostics_changed.is_some(), "{d:?}");
    assert!(!d.is_empty());
}

#[test]
fn same_length_diagnostics_still_say_they_changed() {
    let before = support::fixture();
    let mut after = support::fixture();
    after.diagnostics = before
        .diagnostics
        .iter()
        .cloned()
        .map(|mut d| {
            d.message = format!("rewritten: {}", d.message);
            d
        })
        .collect();

    let d = gearbox_lock::diff(&before, &after);
    // Both reported counts, equal to each other and to what the fixture
    // carries: the rewrite above preserves the length by construction, so
    // comparing the two products' lengths here would be comparing the
    // fixture with itself.
    let (reported_before, reported_after) = d.diagnostics_changed.expect("{d:?}");
    assert_eq!(reported_before, reported_after);
    assert_eq!(reported_before, before.diagnostics.len());

    let summary = d.summary().join("\n");
    assert!(
        summary.contains("diagnostics changed"),
        "same-length content change must not look like a count-only line: {summary}"
    );
}

#[test]
fn detects_an_added_and_a_removed_source() {
    let before = support::fixture();
    let mut after = support::fixture();
    let existing = gearbox_ir::SourceId::new("gears-rust").unwrap();
    let added = gearbox_ir::SourceId::new("house-gears").unwrap();
    let mut source = after.sources[&existing].clone();
    source.id = added.clone();
    after.sources.insert(added.clone(), source);

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(d.sources_added, vec![added.clone()]);
    assert!(!d.is_empty());

    let d = gearbox_lock::diff(&after, &before);
    assert_eq!(d.sources_removed, vec![added]);
    assert!(!d.is_empty());
}

#[test]
fn detects_an_added_and_a_removed_application() {
    let before = support::fixture();
    let mut after = support::fixture();
    let added = ApplicationId::new("reporting").unwrap();
    let mut application = after.applications[0].clone();
    application.name = added.clone();
    after.applications.push(application);

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(d.applications_added, vec![added.clone()]);
    assert!(d.applications_removed.is_empty());
    assert!(!d.is_empty());

    let d = gearbox_lock::diff(&after, &before);
    assert_eq!(d.applications_removed, vec![added]);
    assert!(!d.is_empty());
}

#[test]
fn detects_an_added_and_a_removed_binding() {
    let before = support::fixture();
    let mut after = support::fixture();
    let mut binding = after.bindings[0].clone();
    binding.consumer = GearId::new("api-gateway").unwrap();
    let consumer_application = binding.consumer_application.clone();
    after.bindings.push(binding);

    let expected = gearbox_lock::BindingKey {
        consumer: GearId::new("api-gateway").unwrap(),
        contract: support::payment_api_v1(),
        consumer_application,
    };

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(d.bindings_added, vec![expected.clone()]);
    assert!(!d.is_empty());

    let d = gearbox_lock::diff(&after, &before);
    assert_eq!(d.bindings_removed, vec![expected]);
    assert!(!d.is_empty());
}

#[test]
fn detects_an_added_and_a_removed_cluster_entry() {
    let before = support::fixture();
    let mut after = support::fixture();
    let mut entry = after.cluster[0].clone();
    entry.scope = "reporting".to_owned();
    after.cluster.push(entry);

    let expected = gearbox_lock::ClusterKey {
        scope: "reporting".to_owned(),
        primitive: ClusterPrimitive::Cache,
    };

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(d.cluster_added, vec![expected.clone()]);
    assert!(!d.is_empty());

    let d = gearbox_lock::diff(&after, &before);
    assert_eq!(d.cluster_removed, vec![expected]);
    assert!(!d.is_empty());
}

#[test]
fn detects_an_added_and_a_changed_cut_candidate() {
    let before = support::fixture();
    let mut after = support::fixture();
    let mut candidate = after.cuttable_if_declared[0].clone();
    candidate.provider = GearId::new("api-contracts").unwrap();
    after.cuttable_if_declared.push(candidate);

    let added = gearbox_lock::CutKey {
        consumer: GearId::new("api-gateway").unwrap(),
        provider: GearId::new("api-contracts").unwrap(),
        contract: Some(support::type_registry_v1()),
    };
    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(d.cuts_added, vec![added]);
    assert!(!d.is_empty());

    // Same key, different advice: the blocker moved, and an operator reading
    // the diff needs to see that the candidate they were told about changed.
    let mut changed = support::fixture();
    changed.cuttable_if_declared[0].blocked_by = gearbox_ir::CutBlocker::NoRemoteTransport;
    let d = gearbox_lock::diff(&before, &changed);
    assert_eq!(
        d.cuts_changed,
        vec![gearbox_lock::CutKey {
            consumer: GearId::new("api-gateway").unwrap(),
            provider: GearId::new("types-registry").unwrap(),
            contract: Some(support::type_registry_v1()),
        }]
    );
    assert!(d.cuts_added.is_empty());
    assert!(!d.is_empty());
}

#[test]
fn detects_an_added_and_a_removed_provenance_edge() {
    let before = support::fixture();
    let mut after = support::fixture();
    let added = gearbox_ir::ProvenanceEdge::new(
        gearbox_ir::NodeId::new("gear:audit-archive").unwrap(),
        gearbox_ir::NodeId::new("source:gears-rust").unwrap(),
        gearbox_ir::ProvenanceKind::Declared,
        "audit-archive's gear.gdl was read from the gears-rust source",
    );
    after.provenance.push(added.clone());

    let expected = gearbox_lock::ProvenanceKey {
        from: added.from.clone(),
        kind: added.kind,
        to: added.to,
    };

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(d.provenance_added, vec![expected.clone()]);
    assert!(!d.is_empty());

    let d = gearbox_lock::diff(&after, &before);
    assert_eq!(d.provenance_removed, vec![expected]);
    assert!(!d.is_empty());
}

#[test]
fn a_change_to_a_shadowed_cut_candidate_is_reported() {
    // Two candidates share `(consumer, provider, contract)`: that triple is
    // the sort key, and only fully equal entries are dropped, so both reach
    // the lock. Keying a map on the triple kept the last of them, and a
    // change to the other showed up in no list at all.
    let mut before = support::fixture();
    let mut shadowed = before.cuttable_if_declared[0].clone();
    shadowed.blocked_by = gearbox_ir::CutBlocker::NoRemoteTransport;
    before.cuttable_if_declared.push(shadowed);

    let mut after = before.clone();
    after.cuttable_if_declared[0].file =
        Some(gearbox_ir::RelPath::new("gears/api-gateway/src/lib.rs").unwrap());

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(d.cuts_changed.len(), 1, "{d:?}");
    assert!(!d.is_empty(), "a lock that changed must not read as equal");
}

#[test]
fn a_change_to_a_shadowed_provenance_edge_is_reported() {
    // Same key, different `because`: `dedup` drops only fully equal edges, so
    // both survive canonicalization and the diff has to compare both.
    let mut before = support::fixture();
    let mut shadowed = before.provenance[0].clone();
    shadowed.because = "reached from the other direction".to_owned();
    before.provenance.push(shadowed);

    let mut after = before.clone();
    after.provenance[0].because = "a different reason entirely".to_owned();

    let d = gearbox_lock::diff(&before, &after);
    assert_eq!(d.provenance_changed.len(), 1, "{d:?}");
    assert!(!d.is_empty(), "a lock that changed must not read as equal");
}

#[test]
fn order_inside_nested_collections_is_not_a_change() {
    // The ordinary call has one side straight from the resolver and the other
    // from `read`, which returns a canonicalized product. Ordering inside a
    // nested list is not content, and reporting it as a change made every
    // such pair look modified.
    let before = support::fixture();
    let mut after = support::fixture();

    for application in &mut after.applications {
        application.listens.reverse();
        application.spawns.reverse();
    }
    for gear in after.gears.values_mut() {
        gear.selected_by.reverse();
    }
    for entry in &mut after.cluster {
        entry.requesters.reverse();
    }
    after.applications.reverse();
    after.bindings.reverse();
    after.provenance.reverse();

    let d = gearbox_lock::diff(&before, &after);
    assert!(d.is_empty(), "{d:?}");
    assert!(d.summary().is_empty());
}
